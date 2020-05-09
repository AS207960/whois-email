#[macro_use]
extern crate log;
#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate diesel;
#[macro_use]
extern crate diesel_migrations;

mod proto;
mod server;
mod client;
mod models;
mod schema;
mod sender;

embed_migrations!("migrations");

type DbConn = diesel::r2d2::PooledConnection<diesel::r2d2::ConnectionManager<diesel::pg::PgConnection>>;

#[derive(Clone)]
pub struct Config {
    resolver: std::sync::Arc<trust_dns_resolver::TokioAsyncResolver>,
    connection: diesel::r2d2::Pool<diesel::r2d2::ConnectionManager<diesel::pg::PgConnection>>,
}

pub fn establish_connection() -> diesel::r2d2::Pool<diesel::r2d2::ConnectionManager<diesel::pg::PgConnection>> {
    dotenv::dotenv().ok();

    let database_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set");
    let manager = diesel::r2d2::ConnectionManager::new(&database_url);
    diesel::r2d2::Pool::new(manager)
        .expect(&format!("Error connecting to {}", database_url))
}

lazy_static! {
    pub static ref TEMPLATES: tera::Tera = {
        let tera = match tera::Tera::new("templates/**/*") {
            Ok(t) => t,
            Err(e) => {
                println!("Parsing error(s): {}", e);
                ::std::process::exit(1);
            }
        };
        tera
    };
}


#[tokio::main]
async fn main() {
    pretty_env_logger::init();

    let connection = tokio::task::block_in_place(|| {
        let conn = establish_connection();
        embedded_migrations::run(&conn.get().expect("Error connecting to db")).unwrap();
        info!("DB connection established");
        conn
    });

    let (system_conf, mut system_options) = trust_dns_resolver::system_conf::read_system_conf().expect("Unable to read DNS config");
    system_options.ip_strategy = trust_dns_resolver::config::LookupIpStrategy::Ipv4AndIpv6;
    let resolver = trust_dns_resolver::TokioAsyncResolver::tokio(system_conf, system_options).await.expect("Unable to load DNS config");
    let mut listener = tokio::net::TcpListener::bind("[::]:2525").await.expect("Unable to open listener");

    let config = Config {
        resolver: std::sync::Arc::new(resolver),
        connection,
    };

//    tokio::task::block_in_place(|| {
//        let new_item = models::NewInboundQueueItem {
//            id: &uuid::Uuid::new_v4(),
//            message_id: None,
//            mail_from: &["q@as207960.net"],
//            mail_sender: None,
//            mail_reply_to: None,
//            subject: Some("Test"),
//            contents: &uuid::Uuid::new_v4()
//        };
//
//        diesel::insert_into(schema::inbound_queue::table)
//            .values(&new_item)
//            .get_result::<models::InboundQueueItem>(&connection)
//            .expect("Error saving new item");
//
//        let items = schema::inbound_queue::table.load::<models::InboundQueueItem>(&connection).unwrap();
//        println!("{:?}", items);
//    });

    let mail =
        "Subject: =?utf-8?b?VGVzdPCfpoQ=?=\r\n\
        From: Q <q@relay.as207961.net>\r\n\
        Date: 07 May 2020 18:54:00\r\n\
        Message-ID: <5@relay.as207961.net>\r\n\
        To: <magicalcodewitch@gmail.com>\r\n\
        \r\n\
        bla\u{1F984}".as_bytes();

//    println!("{:?}", client::send_mail("q@relay.as207960.net", &["magicalcodewitch@gmail.com", "test@tools.wormly.com"], mail, &config).await);

    let conf = config.clone();
    tokio::task::spawn(async {
        sender::sending_task(conf).await
    });

    loop {
        let (socket, addr) = listener.accept().await.expect("Unable to accept client");
        println!("new connection from {:?}", addr);
        let conf = config.clone();
        tokio::spawn(async move {
            server::process_socket(socket, conf).await
        });
    }
}