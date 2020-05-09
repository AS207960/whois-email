use tokio::prelude::*;
use std::str::FromStr;
use futures::stream::StreamExt;
use crate::proto::{SMTPResponse, SMTPCommand};

#[derive(Debug, Clone)]
pub enum SendingError {
    InvalidAddress,
    InvalidMessage(String),
    ConnectionError(String),
    TransientError(String),
    PermanentError(String),
}

impl From<std::io::Error> for SendingError {
    fn from(from: std::io::Error) -> Self {
        Self::ConnectionError(from.to_string())
    }
}

impl From<native_tls::Error> for SendingError {
    fn from(from: native_tls::Error) -> Self {
        Self::ConnectionError(from.to_string())
    }
}

#[derive(Debug)]
struct Address {
    local_part: String,
    domain: String,
}

#[derive(Debug)]
struct ForwardPath {
    address: Address,
    mx_addresses: Vec<MXAddress>
}

#[derive(Debug, PartialEq)]
struct MXAddress {
    address: std::net::IpAddr,
    domain: String,
}

#[derive(Debug)]
struct ForwardPaths {
    addresses: Vec<(usize, Address)>,
    mx_addresses: Vec<MXAddress>
}

pub async fn send_mail(reverse_path: &str, forward_paths: &[&str], data: &[u8], config: &crate::Config) -> Vec<Result<(), SendingError>> {
    let mut out_list = Vec::new();
    for _ in 0..forward_paths.len() {
        out_list.push(Ok(()));
    }

    let forward_paths = futures::stream::iter(forward_paths.clone()).then(|p| async move {
        let mut parts = p.rsplitn(2,'@');
        let domain = parts.next().unwrap();
        let local_part = match parts.next() {
            Some(l) => l,
            None => return Err(SendingError::InvalidAddress)
        };
        let mut addr = match std::net::IpAddr::from_str(domain) {
            Ok(a) => vec![MXAddress {
                address: a,
                domain: a.to_string()
            }],
            Err(_) => match config.resolver.mx_lookup(domain).await {
                Ok(r) => {
                    let mut mxs: Vec<_> = r.iter().collect();
                    mxs.sort_by_key(|mx| mx.preference());
                    let addrs = futures::stream::iter(mxs).filter_map(|mx| async move {
                        match config.resolver.lookup_ip(mx.exchange().to_owned()).await {
                            Ok(r) => Some(futures::stream::iter(r.into_iter().map(move |a| MXAddress {
                                address: a,
                                domain: mx.exchange().to_string().trim_end_matches('.').to_string(),
                            }))),
                            Err(_) => None
                        }
                    }).flatten().collect::<Vec<_>>().await;
                    if addrs.is_empty() {
                        return Err(SendingError::InvalidAddress);
                    }
                    addrs
                },
                Err(_) => return Err(SendingError::InvalidAddress)
            }
        };
        addr.sort_by_key(|a| match a.address {
            std::net::IpAddr::V6(_) => 0,
            std::net::IpAddr::V4(_) => 1
        });
        Ok(ForwardPath {
            address: Address {
                local_part: local_part.to_string(),
                domain: domain.to_string(),
            },
            mx_addresses: addr
        })
    }).enumerate().collect::<Vec<_>>().await.into_iter().filter_map(|fp| {
        match fp.1 {
            Ok(v) => {
                Some((fp.0, v))
            },
            Err(e) => {
                out_list[fp.0] = Err(e);
                None
            }
        }
    }).collect::<Vec<_>>();

    let mut forward_paths_grouped: Vec<ForwardPaths> = vec![];
    for forward_path in forward_paths {
        match forward_paths_grouped.iter_mut().find(|p| p.mx_addresses == forward_path.1.mx_addresses) {
            Some(p) => p.addresses.push((forward_path.0, forward_path.1.address)),
            None => forward_paths_grouped.push(ForwardPaths {
                addresses: vec![(forward_path.0, forward_path.1.address)],
                mx_addresses: forward_path.1.mx_addresses
            })
        }
    }

    for forward_path in forward_paths_grouped {
        let indexes = forward_path.addresses.iter().map(|a| a.0).collect::<Vec<_>>();
        let addresses = forward_path.addresses.into_iter().map(|a| a.1).collect::<Vec<_>>();
        let mut last_error = None;
        for mx_address in forward_path.mx_addresses {
            match try_send_mail(reverse_path, &addresses[..], &data, mx_address).await {
                Ok(_) => break,
                Err(SendingError::PermanentError(s)) => {
                    error!("Permanent error sending message: {}", s);
                    last_error = Some(SendingError::PermanentError(s));
                    break;
                },
                Err(SendingError::TransientError(s)) => {
                    warn!("Transient error sending message: {}", s);
                    last_error = Some(SendingError::TransientError(s));
                    break;
                },
                Err(e) => {
                    last_error = Some(e)
                }
            }
        }
        if let Some(last_error) = last_error {
            for index in &indexes {
                out_list[*index] = Err(last_error.clone())
            }
        }
    }

    out_list
}

struct SessionState {
    utf8_support: bool,
    binary_support: bool,
    chunking_support: bool,
    starttls_support: bool,
}

async fn try_send_mail(reverse_path: &str, addresses: &[Address], data: &[u8], mx_address: MXAddress) -> Result<(), SendingError> {
    let s = tokio::net::TcpStream::connect((mx_address.address, 25)).await?;
    let mut stream = tokio::io::BufStream::new(s);

    let data = match mailparse::parse_mail(data) {
        Ok(m) => m,
        Err(e) => return Err(SendingError::InvalidMessage(e.to_string()))
    };


    let banner = SMTPResponse::parse(&mut stream).await.map_err(|e| SendingError::ConnectionError(e))?;
    match banner.code {
        220 => {},
        554 => return Err(SendingError::PermanentError(banner.format_resp())),
        421 => return Err(SendingError::TransientError(banner.format_resp())),
        _ => return Err(SendingError::PermanentError("Bad status code".to_string()))
    }
    info!("Connected to {}", banner.lines[0]);

    let state = handle_helo(&mut stream).await?;

    if state.starttls_support {
        stream.write(SMTPCommand::new("STARTTLS", &[]).to_string().as_bytes()).await?;
        stream.flush().await?;
        let resp = SMTPResponse::parse(&mut stream).await.map_err(|e| SendingError::ConnectionError(e))?;
        match resp.code {
            220 => {
                debug!("STARTTLS response: {}", resp.format_resp());
            },
            500 | 501 => return Err(SendingError::PermanentError(resp.format_resp())),
            421 | 454 => return Err(SendingError::TransientError(resp.format_resp())),
            _ => return Err(SendingError::PermanentError("Bad status code".to_string()))
        }
        let connector: tokio_native_tls::TlsConnector = native_tls::TlsConnector::builder()
            .build()?
            .into();
        let new_stream = connector.connect(&mx_address.domain, stream.into_inner()).await?;
        info!("Connected with STARTTLS");
        let mut stream = tokio::io::BufStream::new(new_stream);

        let state = handle_helo(&mut stream).await?;
        handle_send_mail(&mut stream, reverse_path, addresses, data, &state).await?;
    } else {
        handle_send_mail(&mut stream, reverse_path, addresses, data, &state).await?;
    }

    info!("Email successfully delivered to {}", mx_address.domain);
    Ok(())
}

async fn handle_helo<T: AsyncBufRead + AsyncWrite + std::marker::Unpin>(
    mut stream: &mut T
) -> Result<SessionState, SendingError> {
    let mut state = SessionState {
        utf8_support: false,
        binary_support: false,
        chunking_support: false,
        starttls_support: false,
    };

    stream.write(SMTPCommand::new("EHLO", &["relay-mx.as207960.net"]).to_string().as_bytes()).await?;
    stream.flush().await?;
    let greeting = SMTPResponse::parse(&mut stream).await.map_err(|e| SendingError::ConnectionError(e))?;
    match greeting.code {
        250 => {
            let extensions = &greeting.lines[1..];
            debug!("Greeting: {}", greeting.lines[0]);
            debug!("Extensions:");
            for line in extensions {
                debug!("    {}", line);
            }

            state.utf8_support = extensions.contains(&"8BITMIME".to_string());
            state.binary_support = extensions.contains(&"BINARYMIME".to_string());
            state.chunking_support = extensions.contains(&"CHUNKING".to_string());
            state.starttls_support = extensions.contains(&"STARTTLS".to_string());
        },
        502 => {
            stream.write(SMTPCommand::new("HELO", &["relay-mx.as207960.net"]).to_string().as_bytes()).await?;
            stream.flush().await?;
            let greeting = SMTPResponse::parse(&mut stream).await.map_err(|e| SendingError::ConnectionError(e))?;
            match greeting.code {
                250 => {
                    debug!("Greeting: {}", greeting.lines[0]);
                },
                550 => return Err(SendingError::PermanentError(greeting.format_resp())),
                _ => return Err(SendingError::PermanentError("Bad status code".to_string()))
            }
        }
        500 | 501 | 550 => return Err(SendingError::PermanentError(greeting.format_resp())),
        421 => return Err(SendingError::TransientError(greeting.format_resp())),
        _ => return Err(SendingError::PermanentError("Bad status code".to_string()))
    }

    Ok(state)
}

async fn handle_send_mail<T: AsyncBufRead + AsyncWrite + std::marker::Unpin>(
    mut stream: &mut T, reverse_path: &str, addresses: &[Address], mut data: mailparse::ParsedMail<'_>, state: &SessionState
) -> Result<(), SendingError> {
    let mut args = vec![format!("FROM:<{}>", reverse_path)];
    if state.utf8_support {
        args.push("BODY=8BITMIME".to_string());
    }
    stream.write(SMTPCommand::new("MAIL", &args.iter().map(|x| x.as_ref()).collect::<Vec<_>>()).to_string().as_bytes()).await?;
    stream.flush().await?;
    let resp = SMTPResponse::parse(&mut stream).await.map_err(|e| SendingError::ConnectionError(e))?;
    match resp.code {
        250 => {
            debug!("MAIL response: {}", resp.format_resp());
        },
        500 | 501 | 550 | 552 | 553 | 555 => return Err(SendingError::PermanentError(resp.format_resp())),
        421 | 451 | 452 | 455 => return Err(SendingError::TransientError(resp.format_resp())),
        _ => return Err(SendingError::PermanentError("Bad status code".to_string()))
    }

    for address in addresses {
        stream.write(SMTPCommand::new("RCPT", &[&format!("TO:<{}@{}>", address.local_part, address.domain)]).to_string().as_bytes()).await?;
        stream.flush().await?;
        let resp = SMTPResponse::parse(&mut stream).await.map_err(|e| SendingError::ConnectionError(e))?;
        match resp.code {
            250 | 251 => {
                debug!("RCPT response: {}", resp.format_resp());
            },
            500 | 501 | 550 | 551 | 552 | 553 | 555 | 503 => return Err(SendingError::PermanentError(resp.format_resp())),
            421 | 450 | 451 | 452 | 453 | 455 => return Err(SendingError::TransientError(resp.format_resp())),
            _ => return Err(SendingError::PermanentError("Bad status code".to_string()))
        }
    }

    let body_data = encode_body_part(&state, &mut data);
    if state.chunking_support {
        let mut headers = vec![];
        for header in &data.headers {
            headers.extend(format!("{}: {}\r\n", header.get_key(), encode_header(&header.get_value())).as_bytes());
        }
        headers.extend("\r\n".bytes());
        stream.write(SMTPCommand::new("BDAT", &[&format!("{}", headers.len())]).to_string().as_bytes()).await?;
        stream.write(&headers).await?;
        stream.flush().await?;
        let resp = SMTPResponse::parse(&mut stream).await.map_err(|e| SendingError::ConnectionError(e))?;
        match resp.code {
            250 => {
                debug!("BDAT response: {}", resp.format_resp());
            },
            500 | 501 | 503 | 554 => return Err(SendingError::PermanentError(resp.format_resp())),
            421 => return Err(SendingError::TransientError(resp.format_resp())),
            _ => return Err(SendingError::PermanentError("Bad status code".to_string()))
        }

        stream.write(SMTPCommand::new("BDAT", &[&format!("{}", body_data.len()), "LAST"]).to_string().as_bytes()).await?;
        stream.write(&body_data).await?;
        stream.flush().await?;
        let resp = SMTPResponse::parse(&mut stream).await.map_err(|e| SendingError::ConnectionError(e))?;
        match resp.code {
            250 => {
                debug!("BDAT response: {}", resp.format_resp());
            },
            500 | 501 | 503 | 554 => return Err(SendingError::PermanentError(resp.format_resp())),
            421 => return Err(SendingError::TransientError(resp.format_resp())),
            _ => return Err(SendingError::PermanentError("Bad status code".to_string()))
        }
    } else {
        stream.write(SMTPCommand::new("DATA", &[]).to_string().as_bytes()).await?;
        stream.flush().await?;
        let resp = SMTPResponse::parse(&mut stream).await.map_err(|e| SendingError::ConnectionError(e))?;
        match resp.code {
            354 => {
                debug!("DATA response: {}", resp.format_resp());
            },
            500 | 501 | 503 | 554 => return Err(SendingError::PermanentError(resp.format_resp())),
            421 => return Err(SendingError::TransientError(resp.format_resp())),
            _ => return Err(SendingError::PermanentError("Bad status code".to_string()))
        }
        for header in &data.headers {
            send_data(&mut stream, format!("{}: {}\r\n", header.get_key(), encode_header(&header.get_value())).as_bytes()).await?;
        }
        stream.write(b"\r\n").await?;
        send_data(&mut stream, &body_data).await?;
        stream.write(b"\r\n.\r\n").await?;
        stream.flush().await?;

        let resp = SMTPResponse::parse(&mut stream).await.map_err(|e| SendingError::ConnectionError(e))?;
        debug!("DATA end response: {}", resp.format_resp());
    }

    stream.write(SMTPCommand::new("QUIT", &[]).to_string().as_bytes()).await?;
    stream.flush().await?;
    let resp = SMTPResponse::parse(&mut stream).await.map_err(|e| SendingError::ConnectionError(e))?;
    debug!("QUIT response: {}", resp.format_resp());

    Ok(())
}

async fn send_data<T: AsyncWrite + std::marker::Unpin>(stream: &mut T, data: &[u8]) -> std::io::Result<()> {
    let mut last_3 = [0, '\r' as u8, '\n' as u8];
    for b in data {
        stream.write_u8(*b).await?;
        last_3 = [last_3[1], last_3[2], *b];
        if last_3 == ['\r' as u8, '\n' as u8, '.' as u8] {
            stream.write_u8('.' as u8).await?;
        }
    }
    Ok(())
}

fn encode_body_part(session_state: &SessionState, part: &mut mailparse::ParsedMail) -> Vec<u8> {
    let body = part.get_body_encoded();

    let body_enc = match (session_state.utf8_support, session_state.binary_support, body) {
        (_, _, mailparse::body::Body::Base64(e)) => e.get_raw().to_vec(),
        (_, _, mailparse::body::Body::QuotedPrintable(e)) => e.get_raw().to_vec(),
        (_, _, mailparse::body::Body::SevenBit(e)) => e.get_raw().to_vec(),
        (true, _, mailparse::body::Body::EightBit(e)) => e.get_raw().to_vec(),
        (false, _, mailparse::body::Body::EightBit(e)) => {
            let enc = quoted_printable::encode(e.get_raw());
            part.headers.retain(|h| !h.get_key().eq_ignore_ascii_case("Content-Transfer-Encoding"));
            part.headers.push(mailparse::parse_header(b"Content-Transfer-Encoding: quoted-printable").unwrap().0);
            enc
        },
        (_, true, mailparse::body::Body::Binary(e)) => e.get_raw().to_vec(),
        (_, false, mailparse::body::Body::Binary(e)) => {
            let enc = quoted_printable::encode(e.get_raw());
            part.headers.retain(|h| !h.get_key().eq_ignore_ascii_case("Content-Transfer-Encoding"));
            part.headers.push(mailparse::parse_header(b"Content-Transfer-Encoding: quoted-printable").unwrap().0);
            enc
        },
    };

    body_enc
}

fn encode_header(header: &str) -> String {
    if header.is_ascii() {
        return header.to_string();
    }

    let mut first = true;
    let mut data = header.as_bytes();
    let mut out = String::new();

    loop {
        let len = data.len();
        if !first {
            out.extend("\r\n ".chars());
        }
        let cur_data = if len > 48 {
            let d = data.split_at(48);
            data = d.1;
            d.0
        } else {
            data
        };
        out.extend(format!("=?utf-8?B?{}?=", base64::encode(cur_data)).chars());
        first = false;
        if len <= 48 {
            break;
        }
    }

    println!("{:?} {:?}", header, out);

    out
}