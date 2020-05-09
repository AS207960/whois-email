use super::schema::{inbound_queue, mail_subpart, outbound_message, outbound_queue};
use super::schema;

#[derive(Queryable, Debug)]
pub struct InboundQueueItem {
    pub id: uuid::Uuid,
    pub rcpt_to: String,
    pub message_id: Option<String>,
    pub from: Vec<String>,
    pub sender: Option<String>,
    pub reply_to: Option<Vec<String>>,
    pub subject: Option<String>,
    pub contents_id: uuid::Uuid
}

#[derive(Insertable)]
#[table_name="inbound_queue"]
pub struct NewInboundQueueItem<'a> {
    pub id: &'a uuid::Uuid,
    pub rcpt_to: &'a str,
    pub message_id: Option<&'a str>,
    pub mail_from: &'a[&'a str],
    pub mail_sender: Option<&'a str>,
    pub mail_reply_to: Option<&'a[&'a str]>,
    pub subject: Option<&'a str>,
    pub contents: &'a uuid::Uuid
}

#[derive(Insertable)]
#[table_name="mail_subpart"]
pub struct NewMailSubpart<'a> {
    pub id: &'a uuid::Uuid,
    pub headers: &'a[&'a schema::MailHeader<'a>],
    pub body: &'a[u8],
    pub subparts: &'a[&'a uuid::Uuid]
}

#[derive(Identifiable, Queryable, Debug)]
#[table_name="outbound_message"]
pub struct OutboundMessage {
    pub id: uuid::Uuid,
    pub return_path: String,
    pub data: Vec<u8>
}

#[derive(Insertable)]
#[table_name="outbound_message"]
pub struct NewOutboundMessage<'a> {
    pub id: &'a uuid::Uuid,
    pub return_path: &'a str,
    pub data: &'a[u8],
}

#[derive(Identifiable, Queryable, Associations, Debug)]
#[belongs_to(OutboundMessage, foreign_key = "message_id")]
#[table_name="outbound_queue"]
pub struct OutboundQueueItem {
    pub id: uuid::Uuid,
    pub message_id: uuid::Uuid,
    pub forward_path: String,
    pub state: schema::MailState,
    pub state_since: chrono::DateTime<chrono::Utc>
}

#[derive(Insertable)]
#[table_name="outbound_queue"]
pub struct NewOutboundQueueItem<'a> {
    pub id: &'a uuid::Uuid,
    pub message_id: &'a uuid::Uuid,
    pub forward_path: &'a str,
    pub state: &'a schema::MailState,
    pub state_since: &'a chrono::DateTime<chrono::Utc>,
}