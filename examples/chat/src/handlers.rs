use std::sync::Arc;

use serde::Deserialize;
use socketioxide::{adapter::LocalAdapter, Socket};
use tracing::info;

#[derive(Deserialize, Clone, Debug)]
pub struct Nickname(String);

#[derive(Deserialize)]
pub struct Auth {
    pub nickname: Nickname,
}

pub async fn handler(socket: Arc<Socket<LocalAdapter>>, data: Option<Auth>) {
    info!("Socket connected on / with id: {}", socket.id);
    if let Some(data) = data {
        info!("Nickname: {:?}", data.nickname);
        socket.extensions.insert(data.nickname);
        socket.emit("message", "Welcome to the chat!").ok();
        socket.join("default").unwrap();
    } else {
        info!("No nickname provided, disconnecting...");
        socket.disconnect().ok();
        return;
    }

    socket.on(
        "message",
        |socket, (room, message): (String, String), _, _| async move {
            let Nickname(ref nickname) = *socket.extensions.get().unwrap();
            info!("transfering message from {nickname} to {room}: {message}");
            info!("Sockets in room: {:?}", socket.local().sockets().unwrap());
            if let Some(dest) = socket.to("default").sockets().unwrap().iter().find(|s| {
                s.extensions
                    .get::<Nickname>()
                    .map(|n| n.0 == room)
                    .unwrap_or_default()
            }) {
                info!("Sending message to {}", room);
                dest.emit("message", format!("{}: {}", nickname, message))
                    .ok();
            }

            socket
                .to(room)
                .emit("message", format!("{}: {}", nickname, message))
                .ok();
        },
    );

    socket.on("join", |socket, room: String, _, _| async move {
        info!("Joining room {}", room);
        socket.join(room).unwrap();
    });

    socket.on("leave", |socket, room: String, _, _| async move {
        info!("Leaving room {}", room);
        socket.leave(room).unwrap();
    });

    socket.on("list", |socket, room: Option<String>, _, _| async move {
        if let Some(room) = room {
            info!("Listing sockets in room {}", room);
            let sockets = socket
                .within(room)
                .sockets()
                .unwrap()
                .iter()
                .filter_map(|s| s.extensions.get::<Nickname>())
                .fold("".to_string(), |a, b| a + &b.0 + ", ")
                .trim_end_matches(", ")
                .to_string();
            socket.emit("message", sockets).ok();
        } else {
            let rooms = socket.rooms().unwrap();
            info!("Listing rooms: {:?}", &rooms);
            socket.emit("message", rooms).ok();
        }
    });

    socket.on("nickname", |socket, nickname: Nickname, _, _| async move {
        let previous = socket.extensions.insert(nickname.clone());
        info!("Nickname changed from {:?} to {:?}", &previous, &nickname);
        let msg = format!(
            "{} changed his nickname to {}",
            previous.map(|n| n.0).unwrap_or_default(),
            nickname.0
        );
        socket.to("default").emit("message", msg).ok();
    });

    socket.on_disconnect(|socket, reason| async move {
        info!("Socket disconnected: {} {}", socket.id, reason);
        let Nickname(ref nickname) = *socket.extensions.get().unwrap();
        let msg = format!("{} left the chat", nickname);
        socket.to("default").emit("message", msg).ok();
    });
}
