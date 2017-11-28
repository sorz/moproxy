use std::error::Error;
use std::net::SocketAddr;
use ::rustful::{Server, Context, Response, TreeRouter};

fn index(context: Context, response: Response) {
    response.send("It works!");
}

pub fn run_server(bind: SocketAddr) {
    let router = insert_routes! {
        TreeRouter::new() => {
            "/" => Get: index,
        }
    };
    let server_result = Server {
        handlers: router,
        host: bind.into(),
        threads: Some(1),
        ..Server::default()
    }.run();
    match server_result {
        Ok(_) => (),
        Err(e) => error!("could not start server: {}", e.description())
    };
}

