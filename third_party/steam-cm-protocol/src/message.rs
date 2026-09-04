use std::io::{Cursor, Read};

use bytes::{BufMut, Bytes, BytesMut};
use flate2::read::GzDecoder;
use prost::Message;

use crate::{
    emsg::{EMsg, PROTO_MASK},
    error::{Error, Result},
    protobuf::{CMsgMulti, CMsgProtoBufHeader},
};

pub const NO_JOB_ID: u64 = u64::MAX;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub emsg: u32,
    pub header: CMsgProtoBufHeader,
    pub body: Bytes,
}

impl Packet {
    /// Steam service and PICS responses carry their result in the protobuf
    /// header. Their bodies are often valid empty protobufs on failure, so a
    /// caller that decodes first turns a refusal into a successful empty
    /// answer.
    pub fn ensure_success(&self) -> Result<()> {
        if let Some(result) = self.header.transport_error.filter(|result| *result != 1) {
            return Err(Error::Protocol(format!(
                "Steam transport result {result}: {}",
                self.header.error_message.as_deref().unwrap_or("no detail")
            )));
        }
        // Steam answered and said no. The number is kept rather than printed
        // into a string, because what a caller does about it depends on which
        // number it is — see [`Error::Refused`].
        if let Some(result) = self.header.eresult.filter(|result| *result != 1) {
            return Err(Error::Refused {
                result,
                detail: self.header.error_message.clone(),
            });
        }
        Ok(())
    }

    pub fn jobid_target(&self) -> Option<u64> {
        self.header
            .jobid_target
            .filter(|job_id| *job_id != NO_JOB_ID)
    }

    pub fn jobid_source(&self) -> Option<u64> {
        self.header
            .jobid_source
            .filter(|job_id| *job_id != NO_JOB_ID)
    }

    pub fn target_job_name(&self) -> Option<&str> {
        self.header.target_job_name.as_deref()
    }

    pub fn decode_body<M>(&self) -> Result<M>
    where
        M: Message + Default,
    {
        M::decode(self.body.clone()).map_err(Error::from)
    }
}

pub fn encode_message<M>(emsg: EMsg, header: &CMsgProtoBufHeader, body: &M) -> Result<Bytes>
where
    M: Message,
{
    let body_bytes = body.encode_to_vec();
    encode_raw(emsg.protobuf(), header, &body_bytes)
}

pub fn encode_raw(emsg: u32, header: &CMsgProtoBufHeader, body: &[u8]) -> Result<Bytes> {
    let header_bytes = header.encode_to_vec();
    let mut frame = BytesMut::with_capacity(8 + header_bytes.len() + body.len());
    frame.put_u32_le(emsg);
    frame.put_u32_le(header_bytes.len() as u32);
    frame.extend_from_slice(&header_bytes);
    frame.extend_from_slice(body);
    Ok(frame.freeze())
}

pub fn decode_frame(frame: &[u8]) -> Result<Vec<Packet>> {
    if frame.len() >= 4 {
        let raw_emsg = u32::from_le_bytes(frame[0..4].try_into().expect("slice length checked"));
        if raw_emsg & PROTO_MASK == 0 {
            tracing::trace!(raw_emsg, "skipping non-protobuf frame");
            return Ok(vec![]);
        }
    }
    let packet = decode_packet(frame)?;
    expand_packet(packet)
}

pub fn decode_packet(frame: &[u8]) -> Result<Packet> {
    if frame.len() < 8 {
        return Err(Error::InvalidPacket("frame too short"));
    }

    let raw_emsg = u32::from_le_bytes(frame[0..4].try_into().expect("slice length checked"));
    if raw_emsg & PROTO_MASK == 0 {
        return Err(Error::InvalidPacket("non-protobuf packet is unsupported"));
    }

    let header_len =
        u32::from_le_bytes(frame[4..8].try_into().expect("slice length checked")) as usize;
    if frame.len() < 8 + header_len {
        return Err(Error::InvalidPacket("truncated protobuf header"));
    }

    let header = CMsgProtoBufHeader::decode(&frame[8..8 + header_len])?;
    let body = Bytes::copy_from_slice(&frame[8 + header_len..]);

    Ok(Packet {
        emsg: raw_emsg & !PROTO_MASK,
        header,
        body,
    })
}

fn expand_packet(packet: Packet) -> Result<Vec<Packet>> {
    if packet.emsg != EMsg::Multi.raw() {
        return Ok(vec![packet]);
    }

    let multi = packet.decode_body::<CMsgMulti>()?;
    let payload = multi
        .message_body
        .ok_or(Error::MissingField("CMsgMulti.message_body"))?;

    let data = if multi.size_unzipped.unwrap_or_default() > 0 {
        let mut decoder = GzDecoder::new(payload.as_slice());
        let mut uncompressed = Vec::with_capacity(multi.size_unzipped.unwrap_or_default() as usize);
        decoder.read_to_end(&mut uncompressed)?;
        uncompressed
    } else {
        payload
    };

    split_multi_payload(&data)
}

fn split_multi_payload(payload: &[u8]) -> Result<Vec<Packet>> {
    let mut cursor = Cursor::new(payload);
    let mut packets = Vec::new();

    while (cursor.position() as usize) < payload.len() {
        let mut len_bytes = [0_u8; 4];
        cursor.read_exact(&mut len_bytes)?;
        let packet_len = u32::from_le_bytes(len_bytes) as usize;
        if packet_len == 0 {
            return Err(Error::InvalidPacket("multi payload contained empty packet"));
        }

        let start = cursor.position() as usize;
        let end = start + packet_len;
        if end > payload.len() {
            return Err(Error::InvalidPacket("multi payload packet length overflow"));
        }

        packets.extend(decode_frame(&payload[start..end])?);
        cursor.set_position(end as u64);
    }

    Ok(packets)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::{Compression, write::GzEncoder};

    use bytes::Bytes;

    use super::{Packet, decode_frame, encode_message};
    use crate::{
        emsg::EMsg,
        error::Error,
        protobuf::{CMsgClientHeartBeat, CMsgMulti, CMsgProtoBufHeader},
    };

    #[test]
    fn response_header_errors_are_not_empty_successes() {
        let packet = Packet {
            emsg: EMsg::ServiceMethodResponse.raw(),
            header: CMsgProtoBufHeader {
                eresult: Some(15),
                error_message: Some("access denied".to_owned()),
                ..Default::default()
            },
            body: Bytes::new(),
        };
        assert!(matches!(
            packet.ensure_success(),
            Err(Error::Refused { result: 15, .. })
        ));

        let absent = Packet {
            header: CMsgProtoBufHeader::default(),
            ..packet
        };
        assert!(absent.ensure_success().is_ok());
    }

    #[test]
    fn non_protobuf_frame_is_skipped() {
        // Steam sends legacy (non-protobuf) frames after logon. They should be
        // silently dropped rather than killing the connection.
        let mut frame = Vec::new();
        frame.extend_from_slice(&703u32.to_le_bytes()); // ClientHeartBeat without PROTO_MASK
        frame.extend_from_slice(&[0u8; 32]);
        let result = decode_frame(&frame).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn protobuf_packet_roundtrip() {
        let header = CMsgProtoBufHeader {
            steamid: Some(76561197960287930),
            client_sessionid: Some(42),
            jobid_source: Some(7),
            target_job_name: Some("Authentication.BeginAuthSessionViaQR#1".to_owned()),
            ..Default::default()
        };
        let body = CMsgClientHeartBeat {
            send_reply: Some(true),
        };

        let encoded = encode_message(EMsg::ClientHeartBeat, &header, &body).unwrap();
        let decoded = decode_frame(&encoded).unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].emsg, EMsg::ClientHeartBeat.raw());
        assert_eq!(decoded[0].header.client_sessionid, Some(42));
        assert_eq!(decoded[0].header.jobid_source, Some(7));

        let decoded_body = decoded[0].decode_body::<CMsgClientHeartBeat>().unwrap();
        assert_eq!(decoded_body.send_reply, Some(true));
    }

    #[test]
    fn multi_packet_split_handles_gzip_payload() {
        let packet_a = encode_message(
            EMsg::ClientHeartBeat,
            &CMsgProtoBufHeader {
                jobid_source: Some(1),
                ..Default::default()
            },
            &CMsgClientHeartBeat {
                send_reply: Some(false),
            },
        )
        .unwrap();
        let packet_b = encode_message(
            EMsg::ClientHeartBeat,
            &CMsgProtoBufHeader {
                jobid_source: Some(2),
                ..Default::default()
            },
            &CMsgClientHeartBeat {
                send_reply: Some(true),
            },
        )
        .unwrap();

        let mut payload = Vec::new();
        payload.extend_from_slice(&(packet_a.len() as u32).to_le_bytes());
        payload.extend_from_slice(&packet_a);
        payload.extend_from_slice(&(packet_b.len() as u32).to_le_bytes());
        payload.extend_from_slice(&packet_b);

        let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
        gzip.write_all(&payload).unwrap();
        let compressed = gzip.finish().unwrap();

        let multi = CMsgMulti {
            size_unzipped: Some(payload.len() as u32),
            message_body: Some(compressed),
        };
        let encoded_multi =
            encode_message(EMsg::Multi, &CMsgProtoBufHeader::default(), &multi).unwrap();

        let decoded = decode_frame(&encoded_multi).unwrap();
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].header.jobid_source, Some(1));
        assert_eq!(decoded[1].header.jobid_source, Some(2));
    }
}
