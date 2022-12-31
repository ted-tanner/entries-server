table! {
    blacklisted_tokens (id) {
        id -> Int4,
        token -> Varchar,
        user_id -> Uuid,
        token_expiration_time -> Timestamp,
    }
}

table! {
    buddy_relationships (id) {
        id -> Int4,
        created_timestamp -> Timestamp,
        user1_id -> Uuid,
        user2_id -> Uuid,
    }
}

table! {
    buddy_requests (id) {
        id -> Uuid,
        recipient_user_id -> Uuid,
        sender_user_id -> Uuid,
        accepted -> Bool,
        created_timestamp -> Timestamp,
        accepted_declined_timestamp -> Nullable<Timestamp>,
    }
}

table! {
    budget_share_events (id) {
        id -> Uuid,
        recipient_user_id -> Uuid,
        sender_user_id -> Uuid,
        budget_id -> Uuid,
        accepted -> Bool,
        created_timestamp -> Timestamp,
        accepted_declined_timestamp -> Nullable<Timestamp>,
    }
}

table! {
    budgets (id) {
        id -> Uuid,
        is_shared -> Bool,
        is_private -> Bool,
        is_deleted -> Bool,
        name -> Varchar,
        description -> Nullable<Text>,
        start_date -> Timestamp,
        end_date -> Timestamp,
        latest_entry_time -> Timestamp,
        modified_timestamp -> Timestamp,
        created_timestamp -> Timestamp,
    }
}

table! {
    categories (pk) {
        pk -> Int4,
        budget_id -> Uuid,
        is_deleted -> Bool,
        id -> Int2,
        name -> Varchar,
        limit_cents -> Int8,
        color -> Varchar,
        modified_timestamp -> Timestamp,
        created_timestamp -> Timestamp,
    }
}

table! {
    entries (id) {
        id -> Uuid,
        budget_id -> Uuid,
        user_id -> Nullable<Uuid>,
        is_deleted -> Bool,
        amount_cents -> Int8,
        date -> Timestamp,
        name -> Nullable<Varchar>,
        category -> Nullable<Int2>,
        note -> Nullable<Text>,
        modified_timestamp -> Timestamp,
        created_timestamp -> Timestamp,
    }
}

table! {
    otp_attempts (id) {
        id -> Int4,
        user_id -> Uuid,
        attempt_count -> Int2,
        expiration_time -> Timestamp,
    }
}

table! {
    password_attempts (id) {
        id -> Int4,
        user_id -> Uuid,
        attempt_count -> Int2,
        expiration_time -> Timestamp,
    }
}

table! {
    user_budgets (id) {
        id -> Int4,
        created_timestamp -> Timestamp,
        user_id -> Uuid,
        budget_id -> Uuid,
    }
}

table! {
    user_deletion_requests (id) {
        id -> Int4,
        user_id -> Uuid,
        deletion_request_time -> Timestamp,
        ready_for_deletion_time -> Timestamp,
    }
}

table! {
    user_notifications (id) {
        id -> Uuid,
        user_id -> Uuid,
        is_unread -> Bool,
        is_pristine -> Bool,
        is_deleted -> Bool,
        notification_type -> Int2,
        alt_title -> Varchar,
        alt_message -> Varchar,
        associated_data -> Nullable<Text>,
        modified_timestamp -> Timestamp,
        created_timestamp -> Timestamp,
    }
}

table! {
    user_tombstones (id) {
        id -> Int4,
        user_id -> Uuid,
        deletion_request_time -> Timestamp,
        deletion_time -> Timestamp,
    }
}

table! {
    users (id) {
        id -> Uuid,
        password_hash -> Text,
        is_premium -> Bool,
        premium_expiration -> Nullable<Timestamp>,
        email -> Varchar,
        first_name -> Varchar,
        last_name -> Varchar,
        date_of_birth -> Timestamp,
        currency -> Varchar,
        modified_timestamp -> Timestamp,
        created_timestamp -> Timestamp,
    }
}

allow_tables_to_appear_in_same_query!(
    blacklisted_tokens,
    buddy_relationships,
    buddy_requests,
    budget_share_events,
    budgets,
    categories,
    entries,
    otp_attempts,
    password_attempts,
    user_budgets,
    user_deletion_requests,
    user_notifications,
    user_tombstones,
    users,
);
