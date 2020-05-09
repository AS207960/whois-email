use tokio::prelude::*;
use chrono::prelude::*;
use std::ops::Deref;
use mailparse::MailHeaderMap;

#[derive(Debug)]
pub struct SMTPResponse {
    pub code: u16,
    pub lines: Vec<String>,
}

impl SMTPResponse {
    pub fn new(code: u16, msg: &str) -> Self {
        Self {
            code,
            lines: vec![msg.to_string()],
        }
    }

    pub fn format_resp(&self) -> String {
        format!("Code: {}, Message: {}", self.code, self.lines.join("\r\n"))
    }

    pub fn add_line(&mut self, line: &str) {
        self.lines.push(line.to_string());
    }

    async fn parse_line<T: AsyncBufRead + std::marker::Unpin>(stream: &mut T) -> Result<(u16, bool, String), String> {
        let mut raw_line = String::new();
        let read = match stream.read_line(&mut raw_line).await {
            Ok(l) => l,
            Err(e) => return Err(e.to_string())
        };
        if read == 0 {
            return Err("EOF".to_string())
        }

        let chars = raw_line.chars().into_iter().collect::<Vec<_>>();
        let status_code = match chars[..3].iter().collect::<String>().parse::<u16>() {
            Ok(s) => s,
            Err(e) => return Err(e.to_string())
        };
        let another_line = match chars[3] {
            ' ' => false,
            '-' => true,
            _ => return Err("Invalid character".to_string())
        };
        let line = chars[4..].iter().collect::<String>().trim_end_matches("\r\n").to_string();
        Ok((status_code, another_line, line))
    }

    pub async fn parse<T: AsyncBufRead + std::marker::Unpin>(stream: &mut T) -> Result<Self, String> {
        let (status_code, another_line, line) = SMTPResponse::parse_line(stream).await?;
        let mut out = Self {
            code: status_code,
            lines: vec![line]
        };

        if another_line {
            loop {
                let (status_code, another_line, line) = SMTPResponse::parse_line(stream).await?;
                if status_code != out.code {
                    return Err("Invalid response".to_string())
                }
                out.lines.push(line);
                if !another_line {
                    break
                }
            }
        }

        Ok(out)
    }
}

impl std::fmt::Display for SMTPResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let (last_line, lines) = self.lines.split_last().ok_or_else(|| std::fmt::Error)?;
        for line in lines {
            write!(f, "{}-{}\r\n", self.code, line)?;
        }
        write!(f, "{} {}\r\n", self.code, last_line)
    }
}

pub struct SMTPCommand {
    pub verb: String,
    pub args: Vec<String>,
}

impl SMTPCommand {
    pub fn new(verb: &str, args: &[&str]) -> Self {
        Self {
            verb: verb.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
        }
    }

    pub fn parse(line: &str) -> SMTPCommand {
        let mut parts = line.split_ascii_whitespace();
        Self {
            verb: parts.next().unwrap().to_uppercase(),
            args: parts.map(|s| s.to_string()).rev().collect(),
        }
    }
}

impl std::fmt::Display for SMTPCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.verb)?;
        for arg in &self.args {
            write!(f, " {}", arg)?;
        }
        write!(f, "\r\n")
    }
}

#[derive(Debug)]
pub struct ParsedIMF<'a> {
    pub date: DateTime<Utc>,
    pub from: mailparse::MailAddrList,
    pub sender: Option<mailparse::SingleInfo>,
    pub reply_to: Option<mailparse::MailAddrList>,
    pub to: Option<mailparse::MailAddrList>,
    pub cc: Option<mailparse::MailAddrList>,
    pub bcc: Option<mailparse::MailAddrList>,
    pub subject: Option<String>,
    pub message_id: Option<String>,
    pub in_reply_to: Option<mailparse::MessageIdList>,
    pub references: Option<mailparse::MessageIdList>,
    pub data: mailparse::ParsedMail<'a>,
}

impl ParsedIMF<'_> {
    pub fn mail_from_as_vec(&self) -> Vec<mailparse::SingleInfo> {
        let mut out = vec![];
        for i in self.from.deref() {
            match i {
                mailparse::MailAddr::Single(s) => out.push(s.to_owned()),
                mailparse::MailAddr::Group(g) => out.extend(g.addrs.to_owned())
            }
        }
        out
    }

    pub fn mail_reply_to_as_vec(&self) -> Option<Vec<mailparse::SingleInfo>> {
        match &self.reply_to {
            Some(reply_to) => {
                let mut out = vec![];
                for i in reply_to.deref() {
                    match i {
                        mailparse::MailAddr::Single(s) => out.push(s.to_owned()),
                        mailparse::MailAddr::Group(g) => out.extend(g.addrs.to_owned())
                    }
                }
                Some(out)
            },
            None => None
        }
    }
}

pub fn parse_and_validate_parsed_mail(data: &[u8]) -> Result<ParsedIMF, String> {
    let mail = match mailparse::parse_mail(data) {
        Ok(m) => m,
        Err(e) => return Err(e.to_string())
    };

    let date = match &mail.headers.get_all_values("Date")[..] {
        [d] => {
            let t = match mailparse::dateparse(d) {
                Ok(t) => t,
                Err(e) => return Err(e.to_string())
            };
            Utc.timestamp(t, 0)
        },
        _ => return Err("Invalid number of Date headers".to_string())
    };

    let from = match &mail.headers.get_all_headers("From")[..] {
        [f] => match mailparse::addrparse_header(f) {
            Ok(f) => {
                if f.count_addrs() < 1 {
                    return Err("Invalid number of From addresses".to_string())
                }
                f
            },
            Err(e) => return Err(e.to_string())
        },
        _ => return Err("Invalid number of From headers".to_string())
    };

    let sender = match &mail.headers.get_all_headers("Sender")[..] {
        [f] => match mailparse::addrparse_header(f) {
            Ok(f) => match f.extract_single_info() {
                Some(s) => Some(s),
                None => return Err("Only a single address is allowed as a Sender".to_string())
            },
            Err(e) => return Err(e.to_string())
        },
        _ => if from.count_addrs() != 1 {
            return Err("Invalid number of Sender headers".to_string());
        } else {
            None
        }
    };

    let reply_to = match &mail.headers.get_all_headers("Reply-To")[..] {
        [] => None,
        [f] => match mailparse::addrparse_header(f) {
            Ok(f) => {
                if f.count_addrs() < 1 {
                    return Err("Invalid number of Reply-To addresses".to_string())
                }
                Some(f)
            },
            Err(e) => return Err(e.to_string())
        },
        _ => return Err("Invalid number of Reply-To headers".to_string())
    };

    let to = match &mail.headers.get_all_headers("To")[..] {
        [] => None,
        [f] => match mailparse::addrparse_header(f) {
            Ok(f) => Some(f),
            Err(e) => return Err(e.to_string())
        },
        _ => return Err("Invalid number of To headers".to_string())
    };

    let cc = match &mail.headers.get_all_headers("Cc")[..] {
        [] => None,
        [f] => match mailparse::addrparse_header(f) {
            Ok(f) => Some(f),
            Err(e) => return Err(e.to_string())
        },
        _ => return Err("Invalid number of Cc headers".to_string())
    };

    let bcc = match &mail.headers.get_all_headers("Bcc")[..] {
        [] => None,
        [f] => match mailparse::addrparse_header(f) {
            Ok(f) => Some(f),
            Err(e) => return Err(e.to_string())
        },
        _ => return Err("Invalid number of Bcc headers".to_string())
    };

    let subject = match &mail.headers.get_all_headers("Subject")[..] {
        [] => None,
        [f] => Some(f.get_value()),
        _ => return Err("Invalid number of Subject headers".to_string())
    };

    let message_id = match &mail.headers.get_all_headers("Message-ID")[..] {
        [] => None,
        [f] => match mailparse::msgidparse(&f.get_value()) {
            Ok(mut f) => {
                if f.len() != 1 {
                    return Err("Invalid number of Message-IDs".to_string())
                }
                Some(f.pop().unwrap())
            },
            Err(e) => return Err(e.to_string())
        },
        _ => return Err("Invalid number of Message-ID headers".to_string())
    };

    let in_reply_to = match &mail.headers.get_all_headers("In-Reply-To")[..] {
        [] => None,
        [f] => match mailparse::msgidparse(&f.get_value()) {
            Ok(f) => Some(f),
            Err(e) => return Err(e.to_string())
        },
        _ => return Err("Invalid number of In-Reply-To headers".to_string())
    };

    let references = match &mail.headers.get_all_headers("References")[..] {
        [] => None,
        [f] => match mailparse::msgidparse(&f.get_value()) {
            Ok(f) => Some(f),
            Err(e) => return Err(e.to_string())
        },
        _ => return Err("Invalid number of References headers".to_string())
    };

    Ok(ParsedIMF {
        date,
        from,
        sender,
        reply_to,
        to,
        cc,
        bcc,
        subject,
        message_id,
        in_reply_to,
        references,
        data: mail
    })
}