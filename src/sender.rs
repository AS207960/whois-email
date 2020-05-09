use std::io::Read;
use diesel::prelude::*;
use crate::{schema, models};
use crate::proto::{SMTPResponse};

pub fn queue_confirmation_mail(rcpt_to: &str, mail: &crate::proto::ParsedIMF<'_>, conn: &crate::DbConn) -> Result<(), SMTPResponse> {
    let mut context = tera::Context::new();
    context.insert("rcpt_to", rcpt_to);
    context.insert("subject", &mail.subject);
    context.insert("captcha_link", "https://google.com");

    let content_txt = crate::TEMPLATES.render("confirm_email.txt", &context).unwrap();
    let content_html = crate::TEMPLATES.render("confirm_email.html", &context).unwrap();

    let mut email_builder = lettre_email::Email::builder()
        .from("noreply@relay.as207961.net")
        .date(&time::now())
        .subject(format!("Re: Your email to {}", rcpt_to))
        .alternative(content_html, content_txt);

    if let Some(reply_to) = &mail.reply_to {
        for mb in reply_to.iter() {
            match mb {
                mailparse::MailAddr::Single(s) => {
                    email_builder = email_builder.to(lettre_email::Mailbox {
                        address: s.addr.to_owned(),
                        name: s.display_name.to_owned()
                    })
                }
                mailparse::MailAddr::Group(g) => {
                    for s in &g.addrs {
                        email_builder = email_builder.to(lettre_email::Mailbox {
                            address: s.addr.to_owned(),
                            name: s.display_name.to_owned()
                        })
                    }
                }
            }
        }
    } else {
        for mb in mail.from.iter() {
            match mb {
                mailparse::MailAddr::Single(s) => {
                    email_builder = email_builder.to(lettre_email::Mailbox {
                        address: s.addr.to_owned(),
                        name: s.display_name.to_owned()
                    })
                }
                mailparse::MailAddr::Group(g) => {
                    for s in &g.addrs {
                        email_builder = email_builder.to(lettre_email::Mailbox {
                            address: s.addr.to_owned(),
                            name: s.display_name.to_owned()
                        })
                    }
                }
            }
        }
    }

    if let Some(msg_id) = &mail.message_id {
        email_builder = email_builder.in_reply_to(msg_id.to_owned());
    }
    if let Some(msg_ids) = &mail.references {
        for msg_id in msg_ids.iter() {
            email_builder = email_builder.references(msg_id.to_owned());
        }
    }

    let email: lettre::SendableEmail = email_builder.build().unwrap().into();

    let email_envelope = email.envelope().to_owned();
    let mut email_msg = email.message();

    let message_id = uuid::Uuid::new_v4();

    let mut data = vec![];
    email_msg.read_to_end(&mut data);

    let new_message = crate::models::NewOutboundMessage {
        id: &message_id,
        return_path: email_envelope.from().map(|f| f.as_ref()).unwrap_or_default(),
        data: &data,
    };

    match tokio::task::block_in_place(|| {
        diesel::insert_into(crate::schema::outbound_message::table)
            .values(&new_message)
            .execute(conn)
    }) {
        Ok(_) => {},
        Err(e) => {
            error!("Error creating new message: {}", e);
            return Err(SMTPResponse::new(451, "Internal server error"));
        }
    }

    for forward_path in email_envelope.to() {
        let new_item = crate::models::NewOutboundQueueItem {
            id: &uuid::Uuid::new_v4(),
            message_id: &message_id,
            forward_path: forward_path.as_ref(),
            state: &crate::schema::MailState::Queued,
            state_since: &chrono::Utc::now(),
        };

        match tokio::task::block_in_place(|| {
            diesel::insert_into(crate::schema::outbound_queue::table)
                .values(&new_item)
                .execute(conn)
        }) {
            Ok(_) => {},
            Err(e) => {
                error!("Error inserting message into queue: {}", e);
                return Err(SMTPResponse::new(451, "Internal server error"));
            }
        }
    }

    Ok(())
}

pub async fn sending_task(config: crate::Config) {
    loop {
        let connection = match tokio::task::block_in_place(|| {
            config.connection.get()
        }) {
            Ok(c) => c,
            Err(e) => {
                error!("Error getting DB connection: {}", e);
                tokio::time::delay_for(std::time::Duration::new(5, 0)).await;
                continue
            }
        };

        let mut messages: std::collections::HashMap<uuid::Uuid, models::OutboundMessage> = std::collections::HashMap::new();
        let mut message_forwards: std::collections::HashMap<uuid::Uuid, Vec<models::OutboundQueueItem>> = std::collections::HashMap::new();

        let items = schema::outbound_queue::table
            .filter(schema::outbound_queue::state.eq(schema::MailState::Queued))
            .load::<models::OutboundQueueItem>(&connection).unwrap();

        for item in items {
            let message = schema::outbound_message::table
                .filter(schema::outbound_message::id.eq(item.message_id))
                .first::<models::OutboundMessage>(&connection).unwrap();

            match message_forwards.get_mut(&message.id) {
                Some(m) => m.push(item),
                None => {
                    message_forwards.insert(message.id, vec![item]);
                }
            }
            messages.insert(message.id, message);
        }

        for (id, item) in message_forwards.iter() {
            let data = messages.get(id).unwrap();
            let forward_paths = item.iter().map(|i| i.forward_path.as_str()).collect::<Vec<_>>();

            let results = crate::client::send_mail(&data.return_path, &forward_paths, &data.data, &config).await;
//
//            for (i, res) in item.iter().zip(results.iter()) {
//                println!("{:?} {:?}", i, res);
//            }
        }


//        let data = schema::outbound_message::table.inner_join(schema::outbound_queue::table).select((schema::outbound_message::data,)).load(&connection);

        println!("{:?} {:?}", messages, message_forwards);

        tokio::time::delay_for(std::time::Duration::new(5, 0)).await;
    }
}