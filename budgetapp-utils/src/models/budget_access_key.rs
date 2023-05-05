use diesel::{Insertable, Queryable};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::models::budget::Budget;
use crate::schema::budget_access_keys;

#[derive(Clone, Debug, Serialize, Deserialize, Associations, Identifiable, Queryable)]
#[diesel(belongs_to(Budget, foreign_key = budget_id))]
#[diesel(table_name = budget_access_keys, primary_key(key_id, budget_id))]
pub struct BudgetAccessKey {
    pub key_id: Uuid,
    pub budget_id: Uuid,
    pub public_key: String,
    pub read_only: bool,
}

#[derive(Clone, Debug, Insertable)]
#[diesel(table_name = budget_access_keys, primary_key(key_id, budget_id))]
pub struct NewBudgetAccessKey<'a> {
    pub key_id: Uuid,
    pub budget_id: Uuid,
    pub public_key: &'a str,
    pub read_only: bool,
}
