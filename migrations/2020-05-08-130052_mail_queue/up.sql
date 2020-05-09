create type mail_header as (
    header_key text,
    header_value text
);

create table mail_subpart (
    id uuid primary key,
    headers mail_header[] not null,
    body bytea not null,
    subparts uuid[] not null
);

create table inbound_queue (
    id uuid primary key,
    rcpt_to text not null,
    message_id text,
    mail_from text[] not null,
    mail_sender text,
    mail_reply_to text[],
    subject text,
    contents uuid not null
);

create table outbound_message (
    id uuid primary key,
    return_path text not null,
    data bytea not null
);

create type mail_state as enum ('queued', 'sending', 'sent', 'failed');

create table outbound_queue (
    id uuid primary key,
    message_id uuid not null references outbound_message(id),
    forward_path text not null,
    state mail_state not null,
    state_since timestamp with time zone not null
);

create table registered_addresses (
    id text primary key,
    forward_email text not null
);