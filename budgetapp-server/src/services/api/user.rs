use actix_web::web;

use crate::handlers;

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/user")
            .route("/create", web::post().to(handlers::user::create))
            .route(
                "/verify_creation",
                web::get().to(handlers::user::verify_creation),
            )
            .route(
                "/edit_preferences",
                web::put().to(handlers::user::edit_preferences),
            )
            .route(
                "/change_password",
                web::put().to(handlers::user::change_password),
            )
            .route(
                "/send_buddy_request",
                web::post().to(handlers::user::send_buddy_request),
            )
            .route(
                "/retract_buddy_request",
                web::delete().to(handlers::user::retract_buddy_request),
            )
            .route(
                "/accept_buddy_request",
                web::put().to(handlers::user::accept_buddy_request),
            )
            .route(
                "/decline_buddy_request",
                web::put().to(handlers::user::decline_buddy_request),
            )
            .route(
                "/get_all_pending_buddy_requests_for_user",
                web::get().to(handlers::user::get_all_pending_buddy_requests_for_user),
            )
            .route(
                "/get_all_pending_buddy_requests_made_by_user",
                web::get().to(handlers::user::get_all_pending_buddy_requests_made_by_user),
            )
            .route(
                "/get_buddy_request",
                web::get().to(handlers::user::get_buddy_request),
            )
            .route(
                "/delete_buddy_relationship",
                web::delete().to(handlers::user::delete_buddy_relationship),
            )
            .route("/get_buddies", web::get().to(handlers::user::get_buddies))
            .route("/init_delete", web::put().to(handlers::user::init_delete))
            .route("/delete", web::get().to(handlers::user::delete))
            .route(
                "/is_listed_for_deletion",
                web::get().to(handlers::user::is_listed_for_deletion),
            )
            .route(
                "/cancel_delete",
                web::put().to(handlers::user::cancel_delete),
            ),
    );
}
