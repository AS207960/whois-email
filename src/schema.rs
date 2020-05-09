#[allow(non_camel_case_types)]
#[derive(SqlType)]
#[postgres(type_name = "mail_header")]
pub struct Mail_header;

#[derive(Debug, PartialEq, FromSqlRow, AsExpression)]
#[sql_type = "Mail_header"]
pub struct MailHeader<'a>(pub &'a str, pub &'a str);

impl<'a> diesel::serialize::ToSql<Mail_header, diesel::pg::Pg> for MailHeader<'a> {
    fn to_sql<W: std::io::Write>(&self, out: &mut diesel::serialize::Output<W, diesel::pg::Pg>) -> diesel::serialize::Result {
        diesel::serialize::WriteTuple::<(diesel::sql_types::Text, diesel::sql_types::Text)>::write_tuple(
            &(self.0, self.1),
            out,
        )
    }
}

#[allow(non_camel_case_types)]
#[derive(SqlType)]
#[derive(QueryId)]
#[postgres(type_name = "mail_state")]
pub struct Mail_state;

#[derive(Debug, PartialEq, FromSqlRow, AsExpression)]
#[sql_type = "Mail_state"]
pub enum MailState {
    Queued,
    Sending,
    Sent,
    Failed
}

impl diesel::serialize::ToSql<Mail_state, diesel::pg::Pg> for MailState {
    fn to_sql<W: std::io::Write>(&self, out: &mut diesel::serialize::Output<W, diesel::pg::Pg>) -> diesel::serialize::Result {
        match *self {
            MailState::Queued => out.write_all(b"queued")?,
            MailState::Sending => out.write_all(b"sending")?,
            MailState::Sent => out.write_all(b"sent")?,
            MailState::Failed => out.write_all(b"failed")?,
        }
        Ok(diesel::serialize::IsNull::No)
    }
}

impl diesel::deserialize::FromSql<Mail_state, diesel::pg::Pg> for MailState {
    fn from_sql(bytes: Option<&[u8]>) -> diesel::deserialize::Result<Self> {
        match not_none!(bytes) {
            b"queued" => Ok(MailState::Queued),
            b"sending" => Ok(MailState::Sending),
            b"sent" => Ok(MailState::Sent),
            b"failed" => Ok(MailState::Failed),
            _ => Err("Unrecognized enum variant".into()),
        }
    }
}

table! {
    use diesel::sql_types::*;
    inbound_queue (id) {
        id -> Uuid,
        rcpt_to -> Text,
        message_id -> Nullable<Text>,
        mail_from -> Array<Text>,
        mail_sender -> Nullable<Text>,
        mail_reply_to -> Nullable<Array<Text>>,
        subject -> Nullable<Text>,
        contents -> Uuid,
    }
}

table! {
    use diesel::sql_types::*;
    use super::Mail_header;
    mail_subpart (id) {
        id -> Uuid,
        headers -> Array<Mail_header>,
        body -> Bytea,
        subparts -> Array<Uuid>,
    }
}

table! {
    use diesel::sql_types::*;
    outbound_message (id) {
        id -> Uuid,
        return_path -> Text,
        data -> Bytea,
    }
}

table! {
    use diesel::sql_types::*;
    use super::Mail_state;
    outbound_queue (id) {
        id -> Uuid,
        message_id -> Uuid,
        forward_path -> Text,
        state -> Mail_state,
        state_since -> Timestamptz,
    }
}

table! {
    use diesel::sql_types::*;
    registered_addresses (id) {
        id -> Text,
        forward_email -> Text,
    }
}

joinable!(outbound_queue -> outbound_message (message_id));

allow_tables_to_appear_in_same_query!(
    inbound_queue,
    mail_subpart,
    outbound_message,
    outbound_queue,
    registered_addresses,
);
