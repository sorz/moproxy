use std::error::Error;
use std::net::SocketAddr;
use std::sync::Arc;
use ::rustful::{Server, Context, Response, TreeRouter};
use ::rustful::StatusCode::NotFound;
use ::rustful::header::ContentType;
use ::monitor::ServerList;

fn index(_context: Context, response: Response) {
    response.send("It works!");
}

fn not_found(_context: Context, mut response: Response) {
    response.set_status(NotFound);
    response.send("page not found");
}

fn servers_json(context: Context, mut response: Response) {
    let json = ContentType(content_type!(Text / Json; Charset = Utf8));
    let servers: &Arc<ServerList> = context.global.get()
        .expect("not servers found in global");

    response.headers_mut().set(json);
    response.send(format!("{} servers", servers.get().len()));
}

pub fn run_server(bind: SocketAddr, servers: Arc<ServerList>) {
    let router = insert_routes! {
        TreeRouter::new() => {
            "/" => Get: index as fn(Context, Response),
            "/servers" => Get: servers_json,
            "/*" => Get: not_found,
        }
    };
    let server_result = Server {
        handlers: router,
        host: bind.into(),
        global: Box::new(servers).into(),
        threads: Some(1),
        ..Server::default()
    }.run();
    match server_result {
        Ok(_) => (),
        Err(e) => error!("could not start server: {}", e.description())
    };
}

