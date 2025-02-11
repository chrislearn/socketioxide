//! The polling transport module handles polling, post and init requests
use std::sync::Arc;

use futures::StreamExt;
use http::{Request, Response, StatusCode};
use http_body::Body;

use crate::{
    body::ResponseBody,
    engine::EngineIo,
    errors::Error,
    futures::http_response,
    handler::EngineIoHandler,
    packet::{OpenPacket, Packet},
    service::ProtocolVersion,
    sid::Sid,
    transport::polling::payload::Payload,
    DisconnectReason, SocketReq,
};

use super::TransportType;

mod payload;

pub fn open_req<H, B, R>(
    engine: Arc<EngineIo<H>>,
    protocol: ProtocolVersion,
    req: Request<R>,
    #[cfg(feature = "v3")] supports_binary: bool,
) -> Result<Response<ResponseBody<B>>, Error>
where
    H: EngineIoHandler,
    B: Send + 'static,
{
    let req = SocketReq::from(req.into_parts().0);
    let socket = engine.create_session(
        protocol,
        TransportType::Polling,
        req,
        #[cfg(feature = "v3")]
        supports_binary,
    );

    socket
        .clone()
        .spawn_heartbeat(engine.config.ping_interval, engine.config.ping_timeout);

    let packet = OpenPacket::new(TransportType::Polling, socket.id, &engine.config);

    engine.handler.on_connect(socket);

    let packet: String = Packet::Open(packet).try_into().unwrap();
    let packet = {
        #[cfg(feature = "v3")]
        {
            let mut packet = packet;
            // The V3 protocol requires the packet length to be prepended to the packet.
            // It doesn't use a packet separator like the V4 protocol (and up).
            if protocol == ProtocolVersion::V3 {
                packet = format!("{}:{}", packet.chars().count(), packet);
            }
            packet
        }
        #[cfg(not(feature = "v3"))]
        packet
    };
    http_response(StatusCode::OK, packet, false).map_err(Error::Http)
}

/// Handle http polling request
///
/// If there is packet in the socket buffer, it will be sent immediately
/// Otherwise it will wait for the next packet to be sent from the socket
pub async fn polling_req<B, H>(
    engine: Arc<EngineIo<H>>,
    protocol: ProtocolVersion,
    sid: Sid,
) -> Result<Response<ResponseBody<B>>, Error>
where
    B: Send + 'static,
    H: EngineIoHandler,
{
    let socket = engine.get_socket(sid).ok_or(Error::UnknownSessionID(sid))?;
    if !socket.is_http() {
        return Err(Error::TransportMismatch);
    }

    // If the socket is already locked, it means that the socket is being used by another request
    // In case of multiple http polling, session should be closed
    let rx = match socket.internal_rx.try_lock() {
        Ok(s) => s,
        Err(_) => {
            socket.close(DisconnectReason::MultipleHttpPollingError);
            return Err(Error::HttpErrorResponse(StatusCode::BAD_REQUEST));
        }
    };

    #[cfg(feature = "tracing")]
    tracing::debug!("[sid={sid}] polling request");

    let max_payload = engine.config.max_payload;

    #[cfg(feature = "v3")]
    let Payload { data, has_binary } =
        payload::encoder(rx, protocol, socket.supports_binary, max_payload).await?;
    #[cfg(not(feature = "v3"))]
    let Payload { data, has_binary } = payload::encoder(rx, protocol, max_payload).await?;

    #[cfg(feature = "tracing")]
    tracing::debug!("[sid={sid}] sending data: {:?}", data);
    Ok(http_response(StatusCode::OK, data, has_binary)?)
}

/// Handle http polling post request
///
/// Split the body into packets and send them to the internal socket
pub async fn post_req<R, B, H>(
    engine: Arc<EngineIo<H>>,
    protocol: ProtocolVersion,
    sid: Sid,
    body: Request<R>,
) -> Result<Response<ResponseBody<B>>, Error>
where
    H: EngineIoHandler,
    R: Body + Send + Unpin + 'static,
    <R as Body>::Error: std::fmt::Debug,
    <R as Body>::Data: Send,
    B: Send + 'static,
{
    let socket = engine.get_socket(sid).ok_or(Error::UnknownSessionID(sid))?;
    if !socket.is_http() {
        return Err(Error::TransportMismatch);
    }

    let packets = payload::decoder(body, protocol, engine.config.max_payload);
    futures::pin_mut!(packets);

    while let Some(packet) = packets.next().await {
        match packet {
            Ok(Packet::Close) => {
                #[cfg(feature = "tracing")]
                tracing::debug!("[sid={sid}] closing session");
                socket.send(Packet::Noop)?;
                engine.close_session(sid, DisconnectReason::TransportClose);
                break;
            }
            Ok(Packet::Pong) | Ok(Packet::Ping) => socket
                .heartbeat_tx
                .try_send(())
                .map_err(|_| Error::HeartbeatTimeout),
            Ok(Packet::Message(msg)) => {
                engine.handler.on_message(msg, socket.clone());
                Ok(())
            }
            Ok(Packet::Binary(bin)) | Ok(Packet::BinaryV3(bin)) => {
                engine.handler.on_binary(bin, socket.clone());
                Ok(())
            }
            Ok(p) => {
                #[cfg(feature = "tracing")]
                tracing::debug!("[sid={sid}] bad packet received: {:?}", &p);
                Err(Error::BadPacket(p))
            }
            Err(e) => {
                #[cfg(feature = "tracing")]
                tracing::debug!("[sid={sid}] error parsing packet: {:?}", e);
                engine.close_session(sid, DisconnectReason::PacketParsingError);
                return Err(e);
            }
        }?;
    }
    Ok(http_response(StatusCode::OK, "ok", false)?)
}
