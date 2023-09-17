use entries_utils::messages::{
    AcceptKeyInfo, BudgetAccessTokenList, CategoryId, CategoryUpdate, EncryptedBlobAndCategoryId,
    EncryptedBlobUpdate, EntryAndCategory, EntryId, EntryUpdate, NewBudget, NewEncryptedBlob,
    PublicKey, UserInvitationToBudget,
};
use entries_utils::models::budget_access_key::BudgetAccessKey;
use entries_utils::token::budget_accept_token::BudgetAcceptToken;
use entries_utils::token::budget_access_token::BudgetAccessToken;
use entries_utils::token::budget_invite_sender_token::BudgetInviteSenderToken;
use entries_utils::token::Token;
use entries_utils::{db, db::DaoError, db::DbThreadPool};

use actix_protobuf::{ProtoBuf, ProtoBufResponseBuilder};
use actix_web::{web, HttpResponse};
use ed25519_dalek as ed25519;
use prost::Message;
use rand::rngs::OsRng;
use rsa::{pkcs8::DecodePublicKey, Pkcs1v15Encrypt, RsaPublicKey};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::handlers::error::HttpErrorResponse;
use crate::middleware::auth::{Access, VerifiedToken};
use crate::middleware::special_access_token::SpecialAccessToken;
use crate::middleware::throttle::Throttle;
use crate::middleware::{FromHeader, TokenLocation};

pub async fn get(
    db_thread_pool: web::Data<DbThreadPool>,
    _user_access_token: VerifiedToken<Access, FromHeader>,
    budget_access_token: SpecialAccessToken<BudgetAccessToken, FromHeader>,
) -> Result<HttpResponse, HttpErrorResponse> {
    verify_read_access(&budget_access_token, &db_thread_pool).await?;

    let budget = match web::block(move || {
        let budget_dao = db::budget::Dao::new(&db_thread_pool);
        budget_dao.get_budget(budget_access_token.0.claims.budget_id)
    })
    .await?
    {
        Ok(b) => b,
        Err(e) => match e {
            DaoError::QueryFailure(diesel::result::Error::NotFound) => {
                return Err(HttpErrorResponse::DoesNotExist(
                    "No budget with ID matching token",
                ));
            }
            _ => {
                log::error!("{e}");
                return Err(HttpErrorResponse::InternalError(
                    "Failed to get budget data",
                ));
            }
        },
    };

    Ok(HttpResponse::Ok().protobuf(budget)?)
}

pub async fn get_multiple(
    db_thread_pool: web::Data<DbThreadPool>,
    _user_access_token: VerifiedToken<Access, FromHeader>,
    budget_access_tokens: ProtoBuf<BudgetAccessTokenList>,
) -> Result<HttpResponse, HttpErrorResponse> {
    const INVALID_ID_MSG: &str = "One of the provided budget access tokens had an invalid ID";
    let mut tokens = HashMap::new();
    let mut key_ids = Vec::new();
    let mut budget_ids = Vec::new();

    for token in budget_access_tokens.tokens.iter() {
        let token = BudgetAccessToken::decode(token)
            .map_err(|_| HttpErrorResponse::IncorrectlyFormed(INVALID_ID_MSG))?;

        key_ids.push(token.claims.key_id);
        budget_ids.push(token.claims.budget_id);
        tokens.insert(token.claims.key_id, token);
    }

    let budget_ids = Arc::new(budget_ids);
    let budget_ids_ref = Arc::clone(&budget_ids);

    let budget_dao = db::budget::Dao::new(&db_thread_pool);
    let public_keys = match web::block(move || {
        budget_dao.get_multiple_public_budget_keys(&key_ids, &budget_ids_ref)
    })
    .await?
    {
        Ok(b) => b,
        Err(e) => match e {
            DaoError::QueryFailure(diesel::result::Error::NotFound) => {
                return Err(HttpErrorResponse::DoesNotExist(INVALID_ID_MSG));
            }
            _ => {
                log::error!("{e}");
                return Err(HttpErrorResponse::InternalError(
                    "Failed to get budget data",
                ));
            }
        },
    };

    if public_keys.len() != tokens.len() {
        return Err(HttpErrorResponse::DoesNotExist(INVALID_ID_MSG));
    }

    for key in public_keys {
        let token = match tokens.get(&key.key_id) {
            Some(t) => t,
            None => return Err(HttpErrorResponse::DoesNotExist(INVALID_ID_MSG)),
        };

        token.verify(&key.public_key)?;
    }

    let budgets = match web::block(move || {
        let budget_dao = db::budget::Dao::new(&db_thread_pool);
        budget_dao.get_multiple_budgets_by_id(&budget_ids)
    })
    .await?
    {
        Ok(b) => b,
        Err(e) => match e {
            DaoError::QueryFailure(diesel::result::Error::NotFound) => {
                return Err(HttpErrorResponse::DoesNotExist(
                    "One of the provided IDs did not match a budget",
                ));
            }
            _ => {
                log::error!("{e}");
                return Err(HttpErrorResponse::InternalError(
                    "Failed to get budget data",
                ));
            }
        },
    };

    Ok(HttpResponse::Ok().protobuf(budgets)?)
}

pub async fn create(
    db_thread_pool: web::Data<DbThreadPool>,
    budget_data: ProtoBuf<NewBudget>,
    user_access_token: VerifiedToken<Access, FromHeader>,
    throttle: Throttle<15, 5>,
) -> Result<HttpResponse, HttpErrorResponse> {
    throttle
        .enforce(
            &user_access_token.0.user_id,
            "create_budget",
            &db_thread_pool,
        )
        .await?;

    // temp_id is an ID the client generates that allows the server to differentiate between
    // categories when multiple are sent to the server simultaneously. The server doesn't have any
    // other way of differentiating them because they are encrypted.
    let temp_id_set = HashSet::<i32>::from_iter(budget_data.categories.iter().map(|c| c.temp_id));

    if temp_id_set.len() != budget_data.categories.len() {
        return Err(HttpErrorResponse::InvalidState(
            "Multiple categories with the same ID",
        ));
    }

    let new_budget = match web::block(move || {
        let budget_dao = db::budget::Dao::new(&db_thread_pool);
        budget_dao.create_budget(
            &budget_data.encrypted_blob,
            &budget_data.categories,
            &budget_data.user_public_budget_key,
        )
    })
    .await?
    {
        Ok(b) => b,
        Err(e) => {
            log::error!("{e}");
            return Err(HttpErrorResponse::InternalError("Failed to create budget"));
        }
    };

    Ok(HttpResponse::Created().protobuf(new_budget)?)
}

pub async fn edit(
    db_thread_pool: web::Data<DbThreadPool>,
    _user_access_token: VerifiedToken<Access, FromHeader>,
    budget_access_token: SpecialAccessToken<BudgetAccessToken, FromHeader>,
    budget_data: ProtoBuf<EncryptedBlobUpdate>,
) -> Result<HttpResponse, HttpErrorResponse> {
    verify_read_write_access(&budget_access_token, &db_thread_pool).await?;

    match web::block(move || {
        let budget_dao = db::budget::Dao::new(&db_thread_pool);
        budget_dao.update_budget(
            budget_access_token.0.claims.budget_id,
            &budget_data.encrypted_blob,
            &budget_data.expected_previous_data_hash,
        )
    })
    .await?
    {
        Ok(_) => (),
        Err(e) => match e {
            DaoError::OutOfDateHash => {
                return Err(HttpErrorResponse::OutOfDate("Out of date hash"));
            }
            DaoError::QueryFailure(diesel::result::Error::NotFound) => {
                return Err(HttpErrorResponse::DoesNotExist(
                    "No budget with ID matching token",
                ));
            }
            _ => {
                log::error!("{e}");
                return Err(HttpErrorResponse::InternalError("Failed to edit budget"));
            }
        },
    };

    Ok(HttpResponse::Ok().finish())
}

#[derive(Debug)]
pub struct AcceptKey {
    key_id: Uuid,
    key_id_encrypted: Vec<u8>,
    public_key: Vec<u8>,
    private_key_encrypted: Vec<u8>,
    key_info_encrypted: Vec<u8>,
}

pub async fn invite_user(
    db_thread_pool: web::Data<DbThreadPool>,
    user_access_token: VerifiedToken<Access, FromHeader>,
    budget_access_token: SpecialAccessToken<BudgetAccessToken, FromHeader>,
    invitation_info: ProtoBuf<UserInvitationToBudget>,
    throttle: Throttle<15, 5>,
) -> Result<HttpResponse, HttpErrorResponse> {
    throttle
        .enforce(&user_access_token.0.user_id, "invite_user", &db_thread_pool)
        .await?;

    verify_read_write_access(&budget_access_token, &db_thread_pool).await?;

    if invitation_info.recipient_user_email == user_access_token.0.user_email {
        return Err(HttpErrorResponse::InvalidState(
            "Inviter and recipient are the same",
        ));
    }

    let read_only = invitation_info.read_only;
    let expiration: SystemTime = (&invitation_info.expiration).into();

    let invitation_info = Arc::new(invitation_info.0);
    let invitation_info_ref = Arc::clone(&invitation_info);

    let user_dao = db::user::Dao::new(&db_thread_pool);
    let recipient_public_key = match web::block(move || {
        user_dao.get_user_public_key(&invitation_info_ref.recipient_user_email)
    })
    .await?
    {
        Ok(k) => k,
        Err(e) => match e {
            DaoError::QueryFailure(diesel::result::Error::NotFound) => {
                return Err(HttpErrorResponse::DoesNotExist("No user with given email"));
            }
            _ => {
                log::error!("{e}");
                return Err(HttpErrorResponse::InternalError(
                    "Failed to get recipient user's public key",
                ));
            }
        },
    };

    let (sender, receiver) = oneshot::channel();

    rayon::spawn(move || {
        let accept_key_pair = ed25519::SigningKey::generate(&mut OsRng);
        let accept_public_key = accept_key_pair.verifying_key().to_bytes();
        let accept_private_key = accept_key_pair.to_bytes();

        let recipient_public_key = match RsaPublicKey::from_public_key_der(&recipient_public_key) {
            Ok(k) => k,
            Err(_) => {
                sender
                    .send(Err(HttpErrorResponse::IncorrectlyFormed(
                        "Recipient user's public key is incorrectly formatted",
                    )))
                    .expect("Sending to channel failed");

                return;
            }
        };

        let Ok(private_key_encrypted) =
            recipient_public_key.encrypt(&mut OsRng, Pkcs1v15Encrypt, &accept_private_key[..])
        else {
            sender
                .send(Err(HttpErrorResponse::IncorrectlyFormed(
                    "Recipient user's public key is incorrectly formatted",
                )))
                .expect("Sending to channel failed");

            return;
        };

        let key_id = Uuid::new_v4();

        let key_id_encrypted = recipient_public_key
            .encrypt(&mut OsRng, Pkcs1v15Encrypt, key_id.as_bytes())
            .expect("Failed to encrypt using recipient's public key");

        let key_info = AcceptKeyInfo {
            read_only,
            expiration: expiration
                .duration_since(UNIX_EPOCH)
                .expect("Failed to convert expiration to Unix Epoch time")
                .as_secs(),
        };

        let key_info = key_info.encode_to_vec();

        let key_info_encrypted = recipient_public_key
            .encrypt(&mut OsRng, Pkcs1v15Encrypt, &key_info)
            .expect("Failed to encrypt using recipient's public key");

        sender
            .send(Ok(AcceptKey {
                key_id,
                key_id_encrypted,
                public_key: accept_public_key.to_vec(),
                private_key_encrypted,
                key_info_encrypted,
            }))
            .expect("Sending to channel failed");
    });

    let accept_key_data = receiver.await??;

    let invite_ids = match web::block(move || {
        let budget_dao = db::budget::Dao::new(&db_thread_pool);
        budget_dao.invite_user(
            &invitation_info.recipient_user_email,
            &invitation_info.sender_public_key,
            &invitation_info.encryption_key_encrypted,
            &invitation_info.budget_info_encrypted,
            &invitation_info.sender_info_encrypted,
            &invitation_info.share_info_symmetric_key_encrypted,
            budget_access_token.0.claims.budget_id,
            expiration,
            invitation_info.read_only,
            accept_key_data.key_id,
            &accept_key_data.key_id_encrypted,
            &accept_key_data.public_key,
            &accept_key_data.private_key_encrypted,
            &accept_key_data.key_info_encrypted,
        )
    })
    .await?
    {
        Ok(i) => i,
        Err(e) => match e {
            DaoError::QueryFailure(diesel::result::Error::NotFound) => {
                return Err(HttpErrorResponse::DoesNotExist(
                    "No budget or invite with ID matching token",
                ));
            }
            _ => {
                log::error!("{e}");
                return Err(HttpErrorResponse::InternalError("Failed to share budget"));
            }
        },
    };

    Ok(HttpResponse::Ok().protobuf(invite_ids)?)
}

pub async fn retract_invitation(
    db_thread_pool: web::Data<DbThreadPool>,
    _user_access_token: VerifiedToken<Access, FromHeader>,
    invite_sender_token: SpecialAccessToken<BudgetInviteSenderToken, FromHeader>,
) -> Result<HttpResponse, HttpErrorResponse> {
    let invitation_id = invite_sender_token.0.claims.invite_id;

    let budget_dao = db::budget::Dao::new(&db_thread_pool);
    let invite_sender_public_key =
        match web::block(move || budget_dao.get_budget_invite_sender_public_key(invitation_id))
            .await?
        {
            Ok(k) => k,
            Err(e) => match e {
                DaoError::QueryFailure(diesel::result::Error::NotFound) => {
                    return Err(HttpErrorResponse::DoesNotExist(
                        "No invitation with ID matching token",
                    ));
                }
                _ => {
                    log::error!("{e}");
                    return Err(HttpErrorResponse::InternalError(
                        "Failed to get public budget access key",
                    ));
                }
            },
        };

    invite_sender_token.0.verify(&invite_sender_public_key)?;

    match web::block(move || {
        let budget_dao = db::budget::Dao::new(&db_thread_pool);
        budget_dao.delete_invitation(invite_sender_token.0.claims.invite_id)
    })
    .await?
    {
        Ok(_) => (),
        Err(e) => match e {
            DaoError::QueryFailure(diesel::result::Error::NotFound) => {
                return Err(HttpErrorResponse::DoesNotExist(
                    "No share invite with ID matching token",
                ));
            }
            _ => {
                log::error!("{e}");
                return Err(HttpErrorResponse::InternalError(
                    "Failed to delete invitation",
                ));
            }
        },
    }

    Ok(HttpResponse::Ok().finish())
}

pub async fn accept_invitation(
    db_thread_pool: web::Data<DbThreadPool>,
    user_access_token: VerifiedToken<Access, FromHeader>,
    accept_token: SpecialAccessToken<BudgetAcceptToken, FromHeader>,
    budget_user_public_key: ProtoBuf<PublicKey>,
) -> Result<HttpResponse, HttpErrorResponse> {
    let key_id = accept_token.0.claims.key_id;
    let budget_id = accept_token.0.claims.budget_id;

    let budget_dao = db::budget::Dao::new(&db_thread_pool);
    let budget_accept_key =
        match web::block(move || budget_dao.get_budget_accept_public_key(key_id, budget_id)).await?
        {
            Ok(key) => key,
            Err(e) => match e {
                DaoError::QueryFailure(diesel::result::Error::NotFound) => {
                    return Err(HttpErrorResponse::DoesNotExist(
                        "No share invite with ID matching token",
                    ));
                }
                _ => {
                    log::error!("{e}");
                    return Err(HttpErrorResponse::InternalError(
                        "Failed to accept invitation",
                    ));
                }
            },
        };

    if budget_accept_key.expiration < SystemTime::now() {
        return Err(HttpErrorResponse::OutOfDate("Invitation has expired"));
    }

    accept_token.0.verify(&budget_accept_key.public_key)?;

    let budget_keys = match web::block(move || {
        let budget_dao = db::budget::Dao::new(&db_thread_pool);
        budget_dao.accept_invitation(
            budget_accept_key.key_id,
            budget_accept_key.budget_id,
            budget_accept_key.read_only,
            accept_token.0.claims.invite_id,
            &user_access_token.0.user_email,
            &budget_user_public_key.value,
        )
    })
    .await?
    {
        Ok(key) => key,
        Err(e) => match e {
            DaoError::QueryFailure(diesel::result::Error::NotFound) => {
                return Err(HttpErrorResponse::DoesNotExist(
                    "No share invite with ID matching token",
                ));
            }
            _ => {
                log::error!("{e}");
                return Err(HttpErrorResponse::InternalError(
                    "Failed to accept invitation",
                ));
            }
        },
    };

    Ok(HttpResponse::Ok().protobuf(budget_keys)?)
}

pub async fn decline_invitation(
    db_thread_pool: web::Data<DbThreadPool>,
    user_access_token: VerifiedToken<Access, FromHeader>,
    accept_token: SpecialAccessToken<BudgetAcceptToken, FromHeader>,
) -> Result<HttpResponse, HttpErrorResponse> {
    let key_id = accept_token.0.claims.key_id;
    let budget_id = accept_token.0.claims.budget_id;

    let budget_dao = db::budget::Dao::new(&db_thread_pool);
    let budget_accept_key =
        match web::block(move || budget_dao.get_budget_accept_public_key(key_id, budget_id)).await?
        {
            Ok(key) => key,
            Err(e) => match e {
                DaoError::QueryFailure(diesel::result::Error::NotFound) => {
                    return Err(HttpErrorResponse::DoesNotExist(
                        "No share invite with ID matching token",
                    ));
                }
                _ => {
                    log::error!("{e}");
                    return Err(HttpErrorResponse::InternalError(
                        "Failed to decline invitation",
                    ));
                }
            },
        };

    accept_token.0.verify(&budget_accept_key.public_key)?;

    match web::block(move || {
        let budget_dao = db::budget::Dao::new(&db_thread_pool);
        budget_dao.reject_invitation(
            accept_token.0.claims.invite_id,
            accept_token.0.claims.key_id,
            &user_access_token.0.user_email,
        )
    })
    .await?
    {
        Ok(_) => (),
        Err(e) => match e {
            DaoError::QueryFailure(diesel::result::Error::NotFound) => {
                return Err(HttpErrorResponse::DoesNotExist(
                    "No share invite with ID matching token",
                ));
            }
            _ => {
                log::error!("{e}");
                return Err(HttpErrorResponse::InternalError(
                    "Failed to decline invitation",
                ));
            }
        },
    }

    Ok(HttpResponse::Ok().finish())
}

pub async fn get_all_pending_invitations(
    db_thread_pool: web::Data<DbThreadPool>,
    user_access_token: VerifiedToken<Access, FromHeader>,
) -> Result<HttpResponse, HttpErrorResponse> {
    let invites = match web::block(move || {
        let budget_dao = db::budget::Dao::new(&db_thread_pool);
        budget_dao.get_all_pending_invitations(&user_access_token.0.user_email)
    })
    .await?
    {
        Ok(invites) => invites,
        Err(e) => match e {
            DaoError::QueryFailure(diesel::result::Error::NotFound) => {
                return Ok(HttpResponse::Ok().protobuf(BudgetAccessTokenList::default())?);
            }
            _ => {
                log::error!("{e}");
                return Err(HttpErrorResponse::InternalError(
                    "Failed to find invitations",
                ));
            }
        },
    };

    Ok(HttpResponse::Ok().protobuf(invites)?)
}

pub async fn leave_budget(
    db_thread_pool: web::Data<DbThreadPool>,
    _user_access_token: VerifiedToken<Access, FromHeader>,
    budget_access_token: SpecialAccessToken<BudgetAccessToken, FromHeader>,
) -> Result<HttpResponse, HttpErrorResponse> {
    verify_read_access(&budget_access_token, &db_thread_pool).await?;

    match web::block(move || {
        let budget_dao = db::budget::Dao::new(&db_thread_pool);
        budget_dao.leave_budget(
            budget_access_token.0.claims.budget_id,
            budget_access_token.0.claims.key_id,
        )
    })
    .await?
    {
        Ok(_) => (),
        Err(e) => match e {
            DaoError::QueryFailure(diesel::result::Error::NotFound) => {
                return Err(HttpErrorResponse::DoesNotExist(
                    "No budget with ID matching token",
                ));
            }
            _ => {
                log::error!("{e}");
                return Err(HttpErrorResponse::InternalError(
                    "Failed to remove association with budget",
                ));
            }
        },
    };

    Ok(HttpResponse::Ok().finish())
}

pub async fn create_entry(
    db_thread_pool: web::Data<DbThreadPool>,
    _user_access_token: VerifiedToken<Access, FromHeader>,
    budget_access_token: SpecialAccessToken<BudgetAccessToken, FromHeader>,
    entry_data: ProtoBuf<EncryptedBlobAndCategoryId>,
) -> Result<HttpResponse, HttpErrorResponse> {
    verify_read_write_access(&budget_access_token, &db_thread_pool).await?;

    // Actually optional
    let category_id = entry_data
        .category_id
        .as_ref()
        .map(Uuid::try_from)
        .transpose()?;

    let entry_id = match web::block(move || {
        let budget_dao = db::budget::Dao::new(&db_thread_pool);
        budget_dao.create_entry(
            &entry_data.0.encrypted_blob,
            category_id,
            budget_access_token.0.claims.budget_id,
        )
    })
    .await?
    {
        Ok(id) => id,
        Err(e) => match e {
            DaoError::QueryFailure(diesel::result::Error::NotFound) => {
                return Err(HttpErrorResponse::DoesNotExist(
                    "No budget with ID matching token",
                ));
            }
            DaoError::QueryFailure(diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::ForeignKeyViolation,
                _,
            )) => {
                return Err(HttpErrorResponse::ForeignKeyDoesNotExist(
                    "No category matching ID",
                ))
            }
            _ => {
                log::error!("{e}");
                return Err(HttpErrorResponse::InternalError("Failed to create entry"));
            }
        },
    };

    Ok(HttpResponse::Created().protobuf(EntryId {
        value: entry_id.into(),
    })?)
}

pub async fn create_entry_and_category(
    db_thread_pool: web::Data<DbThreadPool>,
    _user_access_token: VerifiedToken<Access, FromHeader>,
    budget_access_token: SpecialAccessToken<BudgetAccessToken, FromHeader>,
    entry_and_category_data: ProtoBuf<EntryAndCategory>,
) -> Result<HttpResponse, HttpErrorResponse> {
    verify_read_write_access(&budget_access_token, &db_thread_pool).await?;

    let entry_and_category_ids = match web::block(move || {
        let budget_dao = db::budget::Dao::new(&db_thread_pool);
        budget_dao.create_entry_and_category(
            &entry_and_category_data.entry_encrypted_blob,
            &entry_and_category_data.category_encrypted_blob,
            budget_access_token.0.claims.budget_id,
        )
    })
    .await?
    {
        Ok(ids) => ids,
        Err(e) => match e {
            DaoError::QueryFailure(diesel::result::Error::NotFound) => {
                return Err(HttpErrorResponse::DoesNotExist(
                    "No budget with ID matching token",
                ));
            }
            _ => {
                log::error!("{e}");
                return Err(HttpErrorResponse::InternalError("Failed to create entry"));
            }
        },
    };

    Ok(HttpResponse::Created().protobuf(entry_and_category_ids)?)
}

pub async fn edit_entry(
    db_thread_pool: web::Data<DbThreadPool>,
    _user_access_token: VerifiedToken<Access, FromHeader>,
    budget_access_token: SpecialAccessToken<BudgetAccessToken, FromHeader>,
    entry_data: ProtoBuf<EntryUpdate>,
) -> Result<HttpResponse, HttpErrorResponse> {
    verify_read_write_access(&budget_access_token, &db_thread_pool).await?;

    let category_id = entry_data
        .category_id
        .as_ref()
        .map(Uuid::try_from)
        .transpose()?;

    let entry_id = (&entry_data.entry_id).try_into()?;

    match web::block(move || {
        let budget_dao = db::budget::Dao::new(&db_thread_pool);
        budget_dao.update_entry(
            entry_id,
            &entry_data.encrypted_blob,
            &entry_data.expected_previous_data_hash,
            category_id,
            budget_access_token.0.claims.budget_id,
        )
    })
    .await?
    {
        Ok(_) => (),
        Err(e) => match e {
            DaoError::OutOfDateHash => {
                return Err(HttpErrorResponse::OutOfDate("Out of date hash"));
            }
            DaoError::QueryFailure(diesel::result::Error::NotFound) => {
                return Err(HttpErrorResponse::DoesNotExist(
                    "No entry with ID matching token",
                ));
            }
            DaoError::QueryFailure(diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::ForeignKeyViolation,
                _,
            )) => {
                return Err(HttpErrorResponse::ForeignKeyDoesNotExist(
                    "No category matching ID",
                ))
            }
            _ => {
                log::error!("{e}");
                return Err(HttpErrorResponse::InternalError("Failed to update entry"));
            }
        },
    };

    Ok(HttpResponse::Ok().finish())
}

pub async fn delete_entry(
    db_thread_pool: web::Data<DbThreadPool>,
    _user_access_token: VerifiedToken<Access, FromHeader>,
    budget_access_token: SpecialAccessToken<BudgetAccessToken, FromHeader>,
    entry_id: ProtoBuf<EntryId>,
) -> Result<HttpResponse, HttpErrorResponse> {
    verify_read_write_access(&budget_access_token, &db_thread_pool).await?;

    let entry_id = (&entry_id.value).try_into()?;

    match web::block(move || {
        let budget_dao = db::budget::Dao::new(&db_thread_pool);
        budget_dao.delete_entry(entry_id, budget_access_token.0.claims.budget_id)
    })
    .await?
    {
        Ok(id) => id,
        Err(e) => match e {
            DaoError::QueryFailure(diesel::result::Error::NotFound) => {
                return Err(HttpErrorResponse::DoesNotExist(
                    "No entry with ID matching token",
                ));
            }
            _ => {
                log::error!("{e}");
                return Err(HttpErrorResponse::InternalError("Failed to delete entry"));
            }
        },
    };

    Ok(HttpResponse::Ok().finish())
}

pub async fn create_category(
    db_thread_pool: web::Data<DbThreadPool>,
    _user_access_token: VerifiedToken<Access, FromHeader>,
    budget_access_token: SpecialAccessToken<BudgetAccessToken, FromHeader>,
    category_data: ProtoBuf<NewEncryptedBlob>,
) -> Result<HttpResponse, HttpErrorResponse> {
    verify_read_write_access(&budget_access_token, &db_thread_pool).await?;

    let category_id = match web::block(move || {
        let budget_dao = db::budget::Dao::new(&db_thread_pool);
        budget_dao.create_category(&category_data.value, budget_access_token.0.claims.budget_id)
    })
    .await?
    {
        Ok(id) => id,
        Err(e) => match e {
            DaoError::QueryFailure(diesel::result::Error::NotFound) => {
                return Err(HttpErrorResponse::DoesNotExist(
                    "No budget with ID matching token",
                ));
            }
            _ => {
                log::error!("{e}");
                return Err(HttpErrorResponse::InternalError(
                    "Failed to create category",
                ));
            }
        },
    };

    Ok(HttpResponse::Created().protobuf(CategoryId {
        value: category_id.into(),
    })?)
}

pub async fn edit_category(
    db_thread_pool: web::Data<DbThreadPool>,
    _user_access_token: VerifiedToken<Access, FromHeader>,
    budget_access_token: SpecialAccessToken<BudgetAccessToken, FromHeader>,
    category_data: ProtoBuf<CategoryUpdate>,
) -> Result<HttpResponse, HttpErrorResponse> {
    verify_read_write_access(&budget_access_token, &db_thread_pool).await?;

    let category_id = (&category_data.category_id).try_into()?;

    match web::block(move || {
        let budget_dao = db::budget::Dao::new(&db_thread_pool);
        budget_dao.update_category(
            category_id,
            &category_data.encrypted_blob,
            &category_data.expected_previous_data_hash,
            budget_access_token.0.claims.budget_id,
        )
    })
    .await?
    {
        Ok(_) => (),
        Err(e) => match e {
            DaoError::OutOfDateHash => {
                return Err(HttpErrorResponse::OutOfDate("Out of date hash"));
            }
            DaoError::QueryFailure(diesel::result::Error::NotFound) => {
                return Err(HttpErrorResponse::DoesNotExist(
                    "No category with ID matching token",
                ));
            }
            _ => {
                log::error!("{e}");
                return Err(HttpErrorResponse::InternalError(
                    "Failed to update category",
                ));
            }
        },
    };

    Ok(HttpResponse::Ok().finish())
}

pub async fn delete_category(
    db_thread_pool: web::Data<DbThreadPool>,
    _user_access_token: VerifiedToken<Access, FromHeader>,
    budget_access_token: SpecialAccessToken<BudgetAccessToken, FromHeader>,
    category_id: ProtoBuf<CategoryId>,
) -> Result<HttpResponse, HttpErrorResponse> {
    verify_read_write_access(&budget_access_token, &db_thread_pool).await?;

    let category_id = (&category_id.value).try_into()?;

    match web::block(move || {
        let budget_dao = db::budget::Dao::new(&db_thread_pool);
        budget_dao.delete_category(category_id, budget_access_token.0.claims.budget_id)
    })
    .await?
    {
        Ok(id) => id,
        Err(e) => match e {
            DaoError::QueryFailure(diesel::result::Error::NotFound) => {
                return Err(HttpErrorResponse::DoesNotExist(
                    "No category with ID matching token",
                ));
            }
            _ => {
                log::error!("{e}");
                return Err(HttpErrorResponse::InternalError(
                    "Failed to delete category",
                ));
            }
        },
    };

    Ok(HttpResponse::Ok().finish())
}

async fn obtain_public_key(
    key_id: Uuid,
    budget_id: Uuid,
    db_thread_pool: &DbThreadPool,
) -> Result<BudgetAccessKey, HttpErrorResponse> {
    let budget_dao = db::budget::Dao::new(db_thread_pool);
    let key = match web::block(move || budget_dao.get_public_budget_key(key_id, budget_id)).await? {
        Ok(b) => b,
        Err(e) => match e {
            DaoError::QueryFailure(diesel::result::Error::NotFound) => {
                return Err(HttpErrorResponse::DoesNotExist(
                    "No budget with ID matching token",
                ));
            }
            _ => {
                log::error!("{e}");
                return Err(HttpErrorResponse::InternalError(
                    "Failed to get public budget access key",
                ));
            }
        },
    };

    Ok(key)
}

async fn verify_read_write_access<F: TokenLocation>(
    budget_access_token: &SpecialAccessToken<BudgetAccessToken, F>,
    db_thread_pool: &DbThreadPool,
) -> Result<(), HttpErrorResponse> {
    let claims = &budget_access_token.0.claims;
    let public_key = obtain_public_key(claims.key_id, claims.budget_id, db_thread_pool).await?;
    budget_access_token.0.verify(&public_key.public_key)?;

    if public_key.read_only {
        return Err(HttpErrorResponse::ReadOnlyAccess(
            "User has read-only access to budget",
        ));
    }

    Ok(())
}

async fn verify_read_access<F: TokenLocation>(
    budget_access_token: &SpecialAccessToken<BudgetAccessToken, F>,
    db_thread_pool: &DbThreadPool,
) -> Result<(), HttpErrorResponse> {
    let claims = &budget_access_token.0.claims;
    let public_key = obtain_public_key(claims.key_id, claims.budget_id, db_thread_pool).await?;
    budget_access_token.0.verify(&public_key.public_key)?;

    Ok(())
}

#[cfg(test)]
pub mod tests {
    use super::*;

    use entries_utils::messages::{
        Budget as BudgetMessage, BudgetList, EntryIdAndCategoryId, ErrorType, ServerErrorResponse,
    };
    use entries_utils::messages::{BudgetFrame, CategoryWithTempId};
    use entries_utils::models::budget::Budget;
    use entries_utils::schema::budgets::dsl::budgets;

    use actix_protobuf::ProtoBufConfig;
    use actix_web::body::to_bytes;
    use actix_web::http::StatusCode;
    use actix_web::test::{self, TestRequest};
    use actix_web::web::Data;
    use actix_web::App;
    use diesel::{QueryDsl, RunQueryDsl};
    use prost::Message;
    use sha1::{Digest, Sha1};

    use crate::env;
    use crate::handlers::test_utils::{self, gen_budget_token, gen_bytes};

    #[actix_rt::test]
    async fn test_create_and_get_budget_and_entry_and_category() {
        let app = test::init_service(
            App::new()
                .app_data(Data::new(env::testing::DB_THREAD_POOL.clone()))
                .app_data(Data::new(env::testing::SMTP_THREAD_POOL.clone()))
                .app_data(ProtoBufConfig::default())
                .configure(crate::services::api::configure),
        )
        .await;

        let (_, access_token) = test_utils::create_user().await;

        let key_pair = ed25519::SigningKey::generate(&mut OsRng);
        let public_key = key_pair.verifying_key().to_bytes();

        let new_budget = NewBudget {
            encrypted_blob: gen_bytes(32),
            categories: vec![
                CategoryWithTempId {
                    temp_id: 0,
                    encrypted_blob: gen_bytes(40),
                },
                CategoryWithTempId {
                    temp_id: 1,
                    encrypted_blob: gen_bytes(60),
                },
            ],
            user_public_budget_key: Vec::from(public_key),
        };

        let req = TestRequest::post()
            .uri("/api/budget")
            .insert_header(("AccessToken", access_token.as_str()))
            .insert_header(("Content-Type", "application/protobuf"))
            .set_payload(new_budget.encode_to_vec())
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::CREATED);

        let resp_body = to_bytes(resp.into_body()).await.unwrap();
        let mut budget_data = BudgetFrame::decode(resp_body).unwrap();
        budget_data
            .category_ids
            .sort_unstable_by(|a, b| a.temp_id.cmp(&b.temp_id));

        let budget = budgets
            .find(Uuid::try_from(budget_data.id).unwrap())
            .get_result::<Budget>(&mut env::testing::DB_THREAD_POOL.get().unwrap())
            .unwrap();

        let budget_access_token = gen_budget_token(
            budget.id,
            budget_data.access_key_id.try_into().unwrap(),
            &key_pair,
        );

        let category_ids = budget_data
            .category_ids
            .into_iter()
            .map(|c| Uuid::try_from(c.real_id).unwrap())
            .collect::<Vec<_>>();

        let req = TestRequest::get()
            .uri("/api/budget")
            .insert_header(("AccessToken", access_token.as_str()))
            .insert_header(("BudgetAccessToken", budget_access_token.as_str()))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::OK);

        let resp_body = to_bytes(resp.into_body()).await.unwrap();
        let budget_message = BudgetMessage::decode(resp_body).unwrap();

        assert_eq!(Uuid::try_from(budget_message.id).unwrap(), budget.id);
        assert_eq!(budget_message.encrypted_blob, budget.encrypted_blob);

        assert_eq!(budget_message.categories.len(), 2);

        let categories_iter = budget_message
            .categories
            .iter()
            .zip(new_budget.categories.iter());

        let mut initial_categories = Vec::new();
        for (i, (category_message, category)) in categories_iter.enumerate() {
            assert_eq!(
                Uuid::try_from(&category_message.id).unwrap(),
                category_ids[i],
            );
            assert_eq!(category_message.encrypted_blob, category.encrypted_blob);

            initial_categories.push((
                Uuid::try_from(&category_message.id).unwrap(),
                category_message.encrypted_blob.clone(),
            ));
        }

        assert_eq!(budget_message.entries.len(), 0);

        let new_category = NewEncryptedBlob {
            value: gen_bytes(40),
        };

        let req = TestRequest::post()
            .uri("/api/budget/category")
            .insert_header(("AccessToken", access_token.as_str()))
            .insert_header(("BudgetAccessToken", budget_access_token.as_str()))
            .insert_header(("Content-Type", "application/protobuf"))
            .set_payload(new_category.encode_to_vec())
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::CREATED);

        let resp_body = to_bytes(resp.into_body()).await.unwrap();
        let new_category_id: Uuid = CategoryId::decode(resp_body)
            .unwrap()
            .value
            .try_into()
            .unwrap();

        let req = TestRequest::get()
            .uri("/api/budget")
            .insert_header(("AccessToken", access_token.as_str()))
            .insert_header(("BudgetAccessToken", budget_access_token.as_str()))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::OK);

        let resp_body = to_bytes(resp.into_body()).await.unwrap();
        let budget_message = BudgetMessage::decode(resp_body).unwrap();

        assert_eq!(Uuid::try_from(budget_message.id).unwrap(), budget.id);
        assert_eq!(budget_message.encrypted_blob, budget.encrypted_blob);

        assert_eq!(budget_message.categories.len(), 3);

        for category in budget_message.categories.iter() {
            let curr_category_id = (&category.id).try_into().unwrap();

            if curr_category_id == new_category_id {
                assert_eq!(category.encrypted_blob, new_category.value);
            } else {
                let (_, preexisting_category_blob) = initial_categories
                    .iter()
                    .find(|c| c.0 == curr_category_id)
                    .unwrap();

                assert_eq!(preexisting_category_blob, &category.encrypted_blob);
            }
        }

        assert_eq!(budget_message.entries.len(), 0);

        let new_entry = EncryptedBlobAndCategoryId {
            encrypted_blob: gen_bytes(20),
            category_id: Some(new_category_id.into()),
        };

        let req = TestRequest::post()
            .uri("/api/budget/entry")
            .insert_header(("AccessToken", access_token.as_str()))
            .insert_header(("BudgetAccessToken", budget_access_token.as_str()))
            .insert_header(("Content-Type", "application/protobuf"))
            .set_payload(new_entry.encode_to_vec())
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::CREATED);

        let resp_body = to_bytes(resp.into_body()).await.unwrap();
        let new_entry_id: Uuid = EntryId::decode(resp_body)
            .unwrap()
            .value
            .try_into()
            .unwrap();

        let req = TestRequest::get()
            .uri("/api/budget")
            .insert_header(("AccessToken", access_token.as_str()))
            .insert_header(("BudgetAccessToken", budget_access_token.as_str()))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::OK);

        let resp_body = to_bytes(resp.into_body()).await.unwrap();
        let budget_message = BudgetMessage::decode(resp_body).unwrap();

        assert_eq!(Uuid::try_from(budget_message.id).unwrap(), budget.id);
        assert_eq!(budget_message.encrypted_blob, budget.encrypted_blob);

        assert_eq!(budget_message.categories.len(), 3);

        for category in budget_message.categories.iter() {
            let curr_category_id = (&category.id).try_into().unwrap();

            if curr_category_id == new_category_id {
                assert_eq!(category.encrypted_blob, new_category.value);
            } else {
                let (_, preexisting_category_blob) = initial_categories
                    .iter()
                    .find(|c| c.0 == curr_category_id)
                    .unwrap();

                assert_eq!(preexisting_category_blob, &category.encrypted_blob);
            }
        }

        assert_eq!(budget_message.entries.len(), 1);

        assert_eq!(
            Uuid::try_from(&budget_message.entries[0].id).unwrap(),
            new_entry_id,
        );
        assert_eq!(
            Uuid::try_from(budget_message.entries[0].category_id.clone().unwrap()).unwrap(),
            new_category_id,
        );
        assert_eq!(
            budget_message.entries[0].encrypted_blob,
            new_entry.encrypted_blob,
        );

        let new_entry2 = EncryptedBlobAndCategoryId {
            encrypted_blob: gen_bytes(20),
            category_id: None,
        };

        let req = TestRequest::post()
            .uri("/api/budget/entry")
            .insert_header(("AccessToken", access_token.as_str()))
            .insert_header(("BudgetAccessToken", budget_access_token.as_str()))
            .insert_header(("Content-Type", "application/protobuf"))
            .set_payload(new_entry2.encode_to_vec())
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::CREATED);

        let resp_body = to_bytes(resp.into_body()).await.unwrap();
        let new_entry2_id: Uuid = EntryId::decode(resp_body)
            .unwrap()
            .value
            .try_into()
            .unwrap();

        let req = TestRequest::get()
            .uri("/api/budget")
            .insert_header(("AccessToken", access_token.as_str()))
            .insert_header(("BudgetAccessToken", budget_access_token.as_str()))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::OK);

        let resp_body = to_bytes(resp.into_body()).await.unwrap();
        let budget_message = BudgetMessage::decode(resp_body).unwrap();

        assert_eq!(budget_message.entries.len(), 2);

        let first_entry = budget_message
            .entries
            .iter()
            .find(|e| Uuid::try_from(&e.id).unwrap() == new_entry_id)
            .unwrap();
        let second_entry = budget_message
            .entries
            .iter()
            .find(|e| Uuid::try_from(&e.id).unwrap() == new_entry2_id)
            .unwrap();

        assert_eq!(first_entry.encrypted_blob, new_entry.encrypted_blob,);
        assert_eq!(second_entry.encrypted_blob, new_entry2.encrypted_blob,);

        assert_eq!(
            Uuid::try_from(first_entry.category_id.clone().unwrap()).unwrap(),
            new_category_id,
        );

        assert!(second_entry.category_id.is_none());

        let new_entry_and_category = EntryAndCategory {
            entry_encrypted_blob: gen_bytes(30),
            category_encrypted_blob: gen_bytes(12),
        };

        let req = TestRequest::post()
            .uri("/api/budget/entry_and_category")
            .insert_header(("AccessToken", access_token.as_str()))
            .insert_header(("BudgetAccessToken", budget_access_token.as_str()))
            .insert_header(("Content-Type", "application/protobuf"))
            .set_payload(new_entry_and_category.encode_to_vec())
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::CREATED);

        let resp_body = to_bytes(resp.into_body()).await.unwrap();
        let new_entry_and_category_ids = EntryIdAndCategoryId::decode(resp_body).unwrap();

        let new_entry3_id: Uuid = new_entry_and_category_ids.entry_id.try_into().unwrap();
        let new_category4_id: Uuid = new_entry_and_category_ids.category_id.try_into().unwrap();

        let req = TestRequest::get()
            .uri("/api/budget")
            .insert_header(("AccessToken", access_token.as_str()))
            .insert_header(("BudgetAccessToken", budget_access_token.as_str()))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::OK);

        let resp_body = to_bytes(resp.into_body()).await.unwrap();
        let budget_message = BudgetMessage::decode(resp_body).unwrap();

        assert_eq!(Uuid::try_from(budget_message.id).unwrap(), budget.id);
        assert_eq!(budget_message.encrypted_blob, budget.encrypted_blob);

        assert_eq!(budget_message.categories.len(), 4);
        assert_eq!(budget_message.entries.len(), 3);

        let new_category4 = budget_message
            .categories
            .iter()
            .find(|c| Uuid::try_from(&c.id).unwrap() == new_category4_id)
            .unwrap();

        let new_entry3 = budget_message
            .entries
            .iter()
            .find(|e| Uuid::try_from(&e.id).unwrap() == new_entry3_id)
            .unwrap();

        assert_eq!(
            new_category4.encrypted_blob,
            new_entry_and_category.category_encrypted_blob
        );
        assert_eq!(
            new_entry3.encrypted_blob,
            new_entry_and_category.entry_encrypted_blob
        );
        assert_eq!(
            Uuid::try_from(new_entry3.category_id.clone().unwrap()).unwrap(),
            new_category4_id
        );
    }

    #[actix_rt::test]
    async fn test_get_multiple_budgets() {
        let app = test::init_service(
            App::new()
                .app_data(Data::new(env::testing::DB_THREAD_POOL.clone()))
                .app_data(Data::new(env::testing::SMTP_THREAD_POOL.clone()))
                .app_data(ProtoBufConfig::default())
                .configure(crate::services::api::configure),
        )
        .await;

        let (_, access_token) = test_utils::create_user().await;
        let (budget1, budget1_token) = test_utils::create_budget(&access_token).await;
        let (budget2, budget2_token) = test_utils::create_budget(&access_token).await;
        let (budget3, budget3_token) = test_utils::create_budget(&access_token).await;

        let new_entry_and_category = EntryAndCategory {
            entry_encrypted_blob: gen_bytes(30),
            category_encrypted_blob: gen_bytes(12),
        };

        let req = TestRequest::post()
            .uri("/api/budget/entry_and_category")
            .insert_header(("AccessToken", access_token.as_str()))
            .insert_header(("BudgetAccessToken", budget3_token.as_str()))
            .insert_header(("Content-Type", "application/protobuf"))
            .set_payload(new_entry_and_category.encode_to_vec())
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::CREATED);

        let resp_body = to_bytes(resp.into_body()).await.unwrap();
        let new_entry_and_category_ids = EntryIdAndCategoryId::decode(resp_body).unwrap();

        let new_entry_id: Uuid = new_entry_and_category_ids.entry_id.try_into().unwrap();
        let new_category_id: Uuid = new_entry_and_category_ids.category_id.try_into().unwrap();

        let budget_access_tokens = BudgetAccessTokenList {
            tokens: vec![budget1_token, budget2_token, budget3_token],
        };

        let req = TestRequest::get()
            .uri("/api/budget/multiple")
            .insert_header(("AccessToken", access_token.as_str()))
            .insert_header(("Content-Type", "application/protobuf"))
            .set_payload(budget_access_tokens.encode_to_vec())
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::OK);

        let resp_body = to_bytes(resp.into_body()).await.unwrap();
        let budget_list = BudgetList::decode(resp_body).unwrap();

        let resp_budget1 = budget_list
            .budgets
            .iter()
            .find(|b| Uuid::try_from(&b.id).unwrap() == budget1.id)
            .unwrap();
        let resp_budget2 = budget_list
            .budgets
            .iter()
            .find(|b| Uuid::try_from(&b.id).unwrap() == budget2.id)
            .unwrap();
        let resp_budget3 = budget_list
            .budgets
            .iter()
            .find(|b| Uuid::try_from(&b.id).unwrap() == budget3.id)
            .unwrap();

        assert_eq!(budget_list.budgets.len(), 3);

        assert_eq!(Uuid::try_from(resp_budget1.id.clone()).unwrap(), budget1.id);
        assert_eq!(resp_budget1.encrypted_blob, budget1.encrypted_blob);
        assert_eq!(resp_budget1.categories.len(), 0);
        assert_eq!(resp_budget1.entries.len(), 0);

        assert_eq!(Uuid::try_from(resp_budget2.id.clone()).unwrap(), budget2.id);
        assert_eq!(resp_budget2.encrypted_blob, budget2.encrypted_blob);
        assert_eq!(resp_budget2.categories.len(), 0);
        assert_eq!(resp_budget2.entries.len(), 0);

        assert_eq!(Uuid::try_from(resp_budget3.id.clone()).unwrap(), budget3.id);
        assert_eq!(resp_budget3.encrypted_blob, budget3.encrypted_blob);
        assert_eq!(resp_budget3.categories.len(), 1);
        assert_eq!(resp_budget3.entries.len(), 1);

        assert_eq!(
            Uuid::try_from(&resp_budget3.categories[0].id).unwrap(),
            new_category_id
        );
        assert_eq!(
            Uuid::try_from(&resp_budget3.categories[0].budget_id).unwrap(),
            budget3.id
        );
        assert_eq!(
            resp_budget3.categories[0].encrypted_blob,
            new_entry_and_category.category_encrypted_blob
        );

        assert_eq!(
            Uuid::try_from(&resp_budget3.entries[0].id).unwrap(),
            new_entry_id
        );
        assert_eq!(
            Uuid::try_from(&resp_budget3.entries[0].budget_id).unwrap(),
            budget3.id
        );
        assert_eq!(
            Uuid::try_from(resp_budget3.entries[0].category_id.as_ref().unwrap()).unwrap(),
            new_category_id
        );
        assert_eq!(
            resp_budget3.entries[0].encrypted_blob,
            new_entry_and_category.entry_encrypted_blob
        );
    }

    #[actix_rt::test]
    async fn test_deleting_category_sets_category_id_to_null_for_entries() {
        let app = test::init_service(
            App::new()
                .app_data(Data::new(env::testing::DB_THREAD_POOL.clone()))
                .app_data(Data::new(env::testing::SMTP_THREAD_POOL.clone()))
                .app_data(ProtoBufConfig::default())
                .configure(crate::services::api::configure),
        )
        .await;

        let (_, access_token) = test_utils::create_user().await;

        let key_pair = ed25519::SigningKey::generate(&mut OsRng);
        let public_key = key_pair.verifying_key().to_bytes();

        let new_budget = NewBudget {
            encrypted_blob: gen_bytes(32),
            categories: vec![CategoryWithTempId {
                temp_id: 0,
                encrypted_blob: gen_bytes(40),
            }],
            user_public_budget_key: Vec::from(public_key),
        };

        let req = TestRequest::post()
            .uri("/api/budget")
            .insert_header(("AccessToken", access_token.as_str()))
            .insert_header(("Content-Type", "application/protobuf"))
            .set_payload(new_budget.encode_to_vec())
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::CREATED);

        let resp_body = to_bytes(resp.into_body()).await.unwrap();
        let budget_data = BudgetFrame::decode(resp_body).unwrap();

        let budget = budgets
            .find(Uuid::try_from(budget_data.id).unwrap())
            .get_result::<Budget>(&mut env::testing::DB_THREAD_POOL.get().unwrap())
            .unwrap();

        let budget_access_token = gen_budget_token(
            budget.id,
            budget_data.access_key_id.try_into().unwrap(),
            &key_pair,
        );

        let category_id: Uuid = (&budget_data.category_ids[0].real_id).try_into().unwrap();

        let new_entry = EncryptedBlobAndCategoryId {
            encrypted_blob: gen_bytes(20),
            category_id: Some(category_id.into()),
        };

        let req = TestRequest::post()
            .uri("/api/budget/entry")
            .insert_header(("AccessToken", access_token.as_str()))
            .insert_header(("BudgetAccessToken", budget_access_token.as_str()))
            .insert_header(("Content-Type", "application/protobuf"))
            .set_payload(new_entry.encode_to_vec())
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::CREATED);

        let req = TestRequest::get()
            .uri("/api/budget")
            .insert_header(("AccessToken", access_token.as_str()))
            .insert_header(("BudgetAccessToken", budget_access_token.as_str()))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::OK);

        let resp_body = to_bytes(resp.into_body()).await.unwrap();
        let budget_message = BudgetMessage::decode(resp_body).unwrap();

        assert_eq!(Uuid::try_from(budget_message.id).unwrap(), budget.id);
        assert_eq!(budget_message.encrypted_blob, budget.encrypted_blob);

        assert_eq!(budget_message.categories.len(), 1);

        let category_message = &budget_message.categories[0];

        assert_eq!(Uuid::try_from(&category_message.id).unwrap(), category_id,);
        assert_eq!(
            category_message.encrypted_blob,
            new_budget.categories[0].encrypted_blob
        );

        assert_eq!(budget_message.entries.len(), 1);

        let entry_message = &budget_message.entries[0];

        assert_eq!(entry_message.encrypted_blob, new_entry.encrypted_blob);
        assert_eq!(
            Uuid::try_from(new_entry.category_id.clone().unwrap()).unwrap(),
            category_id,
        );

        let category_id_message = CategoryId {
            value: category_id.into(),
        };

        let req = TestRequest::delete()
            .uri("/api/budget/category")
            .insert_header(("AccessToken", access_token.as_str()))
            .insert_header(("BudgetAccessToken", budget_access_token.as_str()))
            .insert_header(("Content-Type", "application/protobuf"))
            .set_payload(category_id_message.encode_to_vec())
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::OK);

        let req = TestRequest::get()
            .uri("/api/budget")
            .insert_header(("AccessToken", access_token.as_str()))
            .insert_header(("BudgetAccessToken", budget_access_token.as_str()))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::OK);

        let resp_body = to_bytes(resp.into_body()).await.unwrap();
        let budget_message = BudgetMessage::decode(resp_body).unwrap();

        assert_eq!(Uuid::try_from(budget_message.id).unwrap(), budget.id);
        assert_eq!(budget_message.encrypted_blob, budget.encrypted_blob);

        assert_eq!(budget_message.categories.len(), 0);
        assert_eq!(budget_message.entries.len(), 1);

        let entry_message = &budget_message.entries[0];

        assert_eq!(entry_message.encrypted_blob, new_entry.encrypted_blob);
        assert!(entry_message.category_id.is_none());
    }

    #[actix_rt::test]
    async fn test_edit_budget() {
        let app = test::init_service(
            App::new()
                .app_data(Data::new(env::testing::DB_THREAD_POOL.clone()))
                .app_data(Data::new(env::testing::SMTP_THREAD_POOL.clone()))
                .app_data(ProtoBufConfig::default())
                .configure(crate::services::api::configure),
        )
        .await;

        let (_, access_token) = test_utils::create_user().await;
        let (budget, budget_token) = test_utils::create_budget(&access_token).await;

        let req = TestRequest::get()
            .uri("/api/budget")
            .insert_header(("AccessToken", access_token.as_str()))
            .insert_header(("BudgetAccessToken", budget_token.as_str()))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::OK);

        let resp_body = to_bytes(resp.into_body()).await.unwrap();
        let budget_message = BudgetMessage::decode(resp_body).unwrap();

        assert_eq!(Uuid::try_from(budget_message.id).unwrap(), budget.id);
        assert_eq!(budget_message.encrypted_blob, budget.encrypted_blob);
        assert_eq!(budget_message.categories.len(), 0);
        assert_eq!(budget_message.entries.len(), 0);

        let blob_update = EncryptedBlobUpdate {
            encrypted_blob: gen_bytes(20),
            expected_previous_data_hash: gen_bytes(20),
        };

        let req = TestRequest::put()
            .uri("/api/budget")
            .insert_header(("AccessToken", access_token.as_str()))
            .insert_header(("BudgetAccessToken", budget_token.as_str()))
            .insert_header(("Content-Type", "application/protobuf"))
            .set_payload(blob_update.encode_to_vec())
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let resp_body = to_bytes(resp.into_body()).await.unwrap();
        let error_message = ServerErrorResponse::decode(resp_body).unwrap();

        assert_eq!(error_message.err_type, ErrorType::OutOfDate as i32);

        let mut sha1_hasher = Sha1::new();
        sha1_hasher.update(budget.encrypted_blob);

        let blob_update = EncryptedBlobUpdate {
            encrypted_blob: gen_bytes(20),
            expected_previous_data_hash: sha1_hasher.finalize().to_vec(),
        };

        let req = TestRequest::put()
            .uri("/api/budget")
            .insert_header(("AccessToken", access_token.as_str()))
            .insert_header(("BudgetAccessToken", budget_token.as_str()))
            .insert_header(("Content-Type", "application/protobuf"))
            .set_payload(blob_update.encode_to_vec())
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::OK);

        let req = TestRequest::get()
            .uri("/api/budget")
            .insert_header(("AccessToken", access_token.as_str()))
            .insert_header(("BudgetAccessToken", budget_token.as_str()))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::OK);

        let resp_body = to_bytes(resp.into_body()).await.unwrap();
        let budget_message = BudgetMessage::decode(resp_body).unwrap();

        assert_eq!(Uuid::try_from(budget_message.id).unwrap(), budget.id);
        assert_eq!(budget_message.encrypted_blob, blob_update.encrypted_blob);
        assert_eq!(budget_message.categories.len(), 0);
        assert_eq!(budget_message.entries.len(), 0);
    }

    // TODO: test_delete_entry (should probably be before last test)

    // TODO: test_edit_entry (very similar to last test)
    // TODO: test_edit_category (very similar to last test)

    // TODO: test_invite_user
    // TODO: test_retract_invitation
    // TODO: test_accept_invitation
    // TODO: test_decline_invitation
    // TODO: test_get_all_pending_invitations
    // TODO: test_leave_budget
}
