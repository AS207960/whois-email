use tokio::prelude::*;
use diesel::prelude::*;
use crate::proto::{SMTPCommand, SMTPResponse};

async fn send_response<T: AsyncWrite + std::marker::Unpin>(socket: &mut T, resp: &SMTPResponse) -> std::io::Result<()> {
    socket.write(resp.to_string().as_bytes()).await?;
    socket.flush().await?;
    Ok(())
}

struct SessionState {
    config: crate::Config,
    client_identity: Option<String>,
    peer_addr: std::net::IpAddr,
    reverse_dns: Option<trust_dns_resolver::Name>,
    protocol: Option<String>,
    reverse_path: Option<String>,
    forward_paths: Vec<String>,
    binary_data: Vec<u8>,
}

impl SessionState {
    fn new(config: crate::Config, peer_addr: std::net::IpAddr, reverse_dns: Option<trust_dns_resolver::Name>) -> Self {
        Self {
            config,
            client_identity: None,
            peer_addr,
            reverse_dns,
            protocol: None,
            reverse_path: None,
            forward_paths: vec![],
            binary_data: vec![]
        }
    }

    fn return_path_header(&self) -> String {
        match &self.reverse_path {
            Some(p) => format!("Return-Path: <{}>\r\n", p),
            None => "Return-Path: <>\r\n".to_string()
        }
    }

    fn received_headers(&self) -> Vec<String> {
        let mut out = "Received: ".to_string();
        if let Some(client_id) = &self.client_identity {
            if let Some(rname) = &self.reverse_dns {
                out.push_str(&format!("FROM {} ({} {})\r\n", client_id, rname.to_ascii(), self.peer_addr));
            } else {
                out.push_str(&format!("FROM {} ({})\r\n", client_id, self.peer_addr));
            }
        }
        out.push_str("    BY relay-mx.as207960.net\r\n");
        out.push_str("    VIA TCP\r\n");
        if let Some(proto) =& self.protocol {
            out.push_str(&format!("    WITH {}\r\n", proto));
        }

        self.forward_paths.iter().map(|path| {
            let mut h = out.clone();
            h.push_str(&format!("    FOR <{}>\r\n", path));
            h
        }).collect()
    }

    async fn handle_mail<T: AsyncWrite + std::marker::Unpin>(&mut self, socket: &mut T, mut cmd: SMTPCommand) -> std::io::Result<()> {
        if self.client_identity.is_none() || self.reverse_path.is_some() {
            return send_response(socket, &SMTPResponse::new(503, "Go read the RFCs")).await;
        }

        let arg = match cmd.args.pop() {
            Some(a) => a,
            None => return send_response(socket, &SMTPResponse::new(501, "Go read the RFCs")).await,
        };

        if arg.starts_with("FROM:") {
            if &arg[5..] == "<>" {
                println!("No reverse path given");
                self.reverse_path = Some("".to_string());
                return send_response(socket, &SMTPResponse::new(250, "OwO? Not giving a reverse path?")).await;
            }

            let e = match mailparse::addrparse(&arg[5..]) {
                Ok(e) => match e.extract_single_info() {
                    Some(e) => e,
                    None => return send_response(socket, &SMTPResponse::new(501, "Go read the RFCs")).await,
                },
                Err(_) => return send_response(socket, &SMTPResponse::new(501, "Go read the RFCs")).await
            };

            println!("Reverse path is {}", e);
            self.reverse_path = Some(e.addr);
            self.forward_paths = vec![];
            send_response(socket, &SMTPResponse::new(250, "OwO what's this? A valid reverse path?")).await
        } else {
            send_response(socket, &SMTPResponse::new(501, "Go read the RFCs")).await
        }
    }

    async fn handle_rcpt<T: AsyncWrite + std::marker::Unpin>(&mut self, socket: &mut T, mut cmd: SMTPCommand) -> std::io::Result<()> {
        if self.client_identity.is_none() || self.reverse_path.is_none() {
            return send_response(socket, &SMTPResponse::new(503, "Go read the RFCs")).await;
        }

        let arg = match cmd.args.pop() {
            Some(a) => a,
            None => return send_response(socket, &SMTPResponse::new(501, "Go read the RFCs")).await,
        };


        if arg.starts_with("TO:") {
            if arg[3..].to_lowercase() == "postmaster" || arg[3..].to_lowercase().starts_with("postmaster@") {
                return send_response(socket, &SMTPResponse::new(551, "Go email noc@as207960.net instead")).await;
            }

            let e = match mailparse::addrparse(&arg[3..]) {
                Ok(e) => match e.extract_single_info() {
                    Some(e) => e,
                    None => {
                        return send_response(socket, &SMTPResponse::new(501, "Go read the RFCs")).await;
                    }
                },
                Err(_) => {
                    return send_response(socket, &SMTPResponse::new(501, "Go read the RFCs")).await;
                }
            };

            let addr = e.addr.rsplit(":").next().unwrap().to_string();

            println!("Forward path is {}", e);
            self.forward_paths.push(addr);
            send_response(socket, &SMTPResponse::new(250, "UwU emails!")).await
        } else {
            send_response(socket, &SMTPResponse::new(501, "Go read the RFCs")).await
        }
    }

    async fn handle_data<T: AsyncBufRead + AsyncWrite + std::marker::Unpin>(&mut self, socket: &mut T, cmd: SMTPCommand) -> std::io::Result<()> {
        if self.client_identity.is_none() || self.reverse_path.is_none() || self.forward_paths.is_empty() {
            return send_response(socket, &SMTPResponse::new(503, "Go read the RFCs")).await;
        }

        if cmd.args.len() != 0 {
            return send_response(socket, &SMTPResponse::new(501, "Go read the RFCs")).await;
        }

        send_response(socket, &SMTPResponse::new(354, "Feed me your email")).await?;

        let mut data = String::new();

        loop {
            let mut line = String::new();
            let read = match socket.read_line(&mut line).await {
                Ok(r) => r,
                Err(e) => match e.kind() {
                    tokio::io::ErrorKind::InvalidData => {
                        send_response(socket, &SMTPResponse::new(553, "UTF8 only please")).await?;
                        continue;
                    },
                    _ => return Err(e)
                }
            };
            if read == 0 {
                return Ok(());
            }

            if line == ".\r\n" {
                break;
            }
            if line.starts_with('.') {
                data.push_str(&line[1..])
            } else {
                data.push_str(&line)
            }
        }

        println!("Mail data is:\r\n{}", data);
        match self.process_email(data.as_bytes()) {
            Ok(_) => {},
            Err(e) => return send_response(socket, &e).await
        }
        send_response(socket, &SMTPResponse::new(250, "Nom nom nom that was delicious")).await?;

        self.reverse_path = None;
        self.forward_paths = vec![];
        self.binary_data = vec![];

        Ok(())
    }

    async fn handle_bdat<T: AsyncBufRead + AsyncWrite + std::marker::Unpin>(&mut self, socket: &mut T, mut cmd: SMTPCommand) -> std::io::Result<()> {
        if self.client_identity.is_none() || self.reverse_path.is_none() || self.forward_paths.is_empty() {
            return send_response(socket, &SMTPResponse::new(503, "Go read the RFCs")).await;
        }

        if cmd.args.len() < 1 || cmd.args.len() > 2 {
            return send_response(socket, &SMTPResponse::new(501, "Go read the RFCs")).await;
        }

        let chunk_size = match cmd.args.pop().unwrap().parse::<usize>() {
            Ok(l) => l,
            Err(_) => return send_response(socket, &SMTPResponse::new(501, "Go read the RFCs")).await
        };

        let is_last = match cmd.args.pop() {
            Some(a) => {
                if a.eq_ignore_ascii_case("LAST") {
                    true
                } else {
                    return send_response(socket, &SMTPResponse::new(501, "Go read the RFCs")).await;
                }
            },
            None => chunk_size == 0
        };

        let mut buffer =  vec![0u8; chunk_size];
        socket.read_exact(&mut buffer).await?;

        self.binary_data.extend(&buffer);

        if is_last {
            println!("Mail data is:\r\n{:?}", self.binary_data);
            match self.process_email(&self.binary_data) {
                Ok(_) => send_response(socket, &SMTPResponse::new(250, "Nom nom nom that was delicious")).await?,
                Err(e) => send_response(socket, &e).await?
            }
            self.reverse_path = None;
            self.forward_paths = vec![];
            self.binary_data = vec![];

            Ok(())
        } else {
            send_response(socket, &SMTPResponse::new(250, "Nom nom nom more please!")).await
        }
    }

    fn process_email(&self, data: &[u8]) -> Result<(), SMTPResponse> {
        for (recipient, received_header) in self.forward_paths.iter().zip(self.received_headers().iter()) {
            let header_data = format!("{}{}", self.return_path_header(), received_header);
            let mut idv_data = header_data.as_bytes().to_vec();
            idv_data.extend(data);
            let parsed_imf = match crate::proto::parse_and_validate_parsed_mail(&idv_data) {
                Ok(p) => p,
                Err(e) => {
                    let mut resp = SMTPResponse::new(550, "Ew! Non RFC5322 compliant mail!");
                    resp.add_line(&e);
                    return Err(resp);
                }
            };

            let conn = match tokio::task::block_in_place(|| {
                self.config.connection.get()
            }) {
                Ok(c) => c,
                Err(e) => {
                    error!("Error getting database connection: {}", e);
                    return Err(SMTPResponse::new(451, "Internal server error"));
                }
            };

            let queue_id = uuid::Uuid::new_v4();
            let contents_id = self.process_email_part(&parsed_imf.data, &conn)?;

            let mail_from = parsed_imf.mail_from_as_vec().iter().map(|f| f.to_string()).collect::<Vec<_>>();
            let mail_sender = match &parsed_imf.sender {
                Some(s) => Some(s.to_string()),
                None => None
            };
            let mail_reply_to = match parsed_imf.mail_reply_to_as_vec() {
                Some(r) => Some(r.iter().map(|f| f.to_string()).collect::<Vec<_>>()),
                None => None
            };
            let mut mail_reply_to_deref = vec![];
            let mail_reply_to_2 = match &mail_reply_to {
                Some(reply_to) => {
                    for r in reply_to {
                        mail_reply_to_deref.push(r.as_str())
                    }
                    Some(mail_reply_to_deref)
                },
                None => None
            };

            let new_item = crate::models::NewInboundQueueItem {
                id: &queue_id,
                rcpt_to: recipient,
                message_id: parsed_imf.message_id.as_deref(),
                mail_from: &mail_from.iter().map(|x| x.as_ref()).collect::<Vec<_>>(),
                mail_sender: mail_sender.as_deref(),
                mail_reply_to: match &mail_reply_to_2 {
                    Some(x) => Some(x.as_ref()),
                    None => None
                },
                subject: parsed_imf.subject.as_deref(),
                contents: &contents_id
            };

            match tokio::task::block_in_place(|| {
                diesel::insert_into(crate::schema::inbound_queue::table)
                    .values(&new_item)
                    .execute(&conn)
            }) {
                Ok(_) => {},
                Err(e) => {
                    error!("Error inserting into queue: {}", e);
                    return Err(SMTPResponse::new(451, "Internal server error"));
                }
            }

            tokio::task::block_in_place(|| {
                crate::sender::queue_confirmation_mail(&recipient, &parsed_imf, &conn)
            })?;
        }

        Ok(())
    }

    fn process_email_part(&self, part: &mailparse::ParsedMail<'_>, conn: &crate::DbConn) -> Result<uuid::Uuid, SMTPResponse> {
        let contents_id = uuid::Uuid::new_v4();

        let body = match part.get_body_raw() {
            Ok(b) => b,
            Err(_) => return Err(SMTPResponse::new(550, "Error decoding content transfer encoding"))
        };

        let subparts: Vec<_> = part.subparts.iter().map(|s| {
            self.process_email_part(s, conn)
        }).collect::<Result<Vec<_>, _>>()?;

        let mut headers = vec![];
        let headers_1 = part.headers.iter().map(|h| (h.get_key(), h.get_value())).collect::<Vec<_>>();
        for h in &headers_1 {
            headers.push(crate::schema::MailHeader(&h.0, &h.1));
        }

        let new_subpart = crate::models::NewMailSubpart {
            id: &contents_id,
            headers: &headers.iter().map(|x| x).collect::<Vec<_>>(),
            body: body.as_ref(),
            subparts: &subparts.iter().map(|x| x).collect::<Vec<_>>()
        };

        match tokio::task::block_in_place(|| {
            diesel::insert_into(crate::schema::mail_subpart::table)
                .values(&new_subpart)
                .execute(conn)
        }) {
            Ok(_) => {},
            Err(e) => {
                error!("Error inserting into queue: {}", e);
                return Err(SMTPResponse::new(451, "Internal server error"));
            }
        }

        Ok(contents_id)
    }
}

pub async fn process_socket(s: tokio::net::TcpStream, config: crate::Config) -> std::io::Result<()> {
    let peer_ip = s.peer_addr()?.ip();
    let mut socket = tokio::io::BufStream::new(s);

    let peer_hostname = match config.resolver.reverse_lookup(peer_ip.clone()).await.ok() {
        Some(r) => r.into_iter().next(),
        None => None
    };

    let mut session_state = SessionState::new(config.clone(),peer_ip.clone(), peer_hostname.clone());

    send_response(&mut socket, &SMTPResponse::new(220, "relay-mx.as207960.net Hippity hoppity your mail is now my property")).await?;

    loop {
        let mut line = String::new();
        let read = match socket.read_line(&mut line).await {
            Ok(r) => r,
            Err(e) => match e.kind() {
                tokio::io::ErrorKind::InvalidData => {
                    send_response(&mut socket, &SMTPResponse::new(553, "UTF8 only please")).await?;
                    continue;
                },
                _ => return Err(e)
            }
        };
        if read == 0 {
            break;
        }

        let mut cmd = SMTPCommand::parse(&line);

        match cmd.verb.as_str() {
            "HELO" => if cmd.args.len() != 1 {
                send_response(&mut socket, &SMTPResponse::new(501, "Go read the RFCs")).await?;
            } else {
                let name = cmd.args.pop().unwrap();
                println!("HELO from {}", name);
                session_state.client_identity = Some(name);
                session_state.protocol = Some("SMTP".to_string());
                session_state.reverse_path = None;
                session_state.forward_paths = vec![];
                send_response(&mut socket, &SMTPResponse::new(250, &format!("relay-mx.as207960.net Good day to you {}", match &peer_hostname {
                    Some(d) => d.to_ascii(),
                    None => peer_ip.to_string()
                }))).await?;
            }
            "EHLO" => if cmd.args.len() != 1 {
                send_response(&mut socket, &SMTPResponse::new(501, "Go read the RFCs")).await?;
            } else {
                let name = cmd.args.pop().unwrap();
                println!("EHLO from {}", name);
                session_state.client_identity = Some(name);
                session_state.protocol = Some("ESMTP".to_string());
                session_state.reverse_path = None;
                session_state.forward_paths = vec![];
                let mut resp = SMTPResponse::new(250, &format!("relay-mx.as207960.net Good day to you {}", match &peer_hostname {
                    Some(d) => d.to_ascii(),
                    None => peer_ip.to_string()
                }));
                resp.add_line("8BITMIME");
                resp.add_line("SMTPUTF8");
                resp.add_line("CHUNKING");
                resp.add_line("SIZE 0");
                send_response(&mut socket, &resp).await?;
            }
            "MAIL" => session_state.handle_mail(&mut socket, cmd).await?,
            "RCPT" => session_state.handle_rcpt(&mut socket, cmd).await?,
            "DATA" => session_state.handle_data(&mut socket, cmd).await?,
            "BDAT" => session_state.handle_bdat(&mut socket, cmd).await?,
            "RSET" => {
                session_state.reverse_path = None;
                session_state.forward_paths = vec![];
                session_state.binary_data = vec![];
                send_response(&mut socket, &SMTPResponse::new(250, "And so we begin again")).await?;
            }
            "NOOP" => send_response(&mut socket, &SMTPResponse::new(250, "Well that was a waste")).await?,
            "HELP" | "EXPN" => send_response(&mut socket, &SMTPResponse::new(502, "No")).await?,
            "QUIT" => {
                send_response(&mut socket, &SMTPResponse::new(221, "Toodles!")).await?;
                break;
            }
            _ => send_response(&mut socket, &SMTPResponse::new(500, "Go read the RFCs")).await?,
        }
    }

    Ok(())
}