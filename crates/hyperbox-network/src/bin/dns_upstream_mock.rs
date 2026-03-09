use std::net::SocketAddr;

use hickory_proto::{
    op::{Message, MessageType, OpCode, ResponseCode},
    rr::{RData, Record, RecordType, rdata::A},
    serialize::binary::{BinDecodable, BinDecoder, BinEncodable, BinEncoder},
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let listen: SocketAddr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:55353".to_string())
        .parse()?;

    let socket = tokio::net::UdpSocket::bind(listen).await?;
    println!("mock upstream dns listening on {listen}");

    loop {
        let mut buf = vec![0u8; 4096];
        let (len, peer) = socket.recv_from(&mut buf).await?;
        let packet = &buf[..len];

        let mut dec = BinDecoder::new(packet);
        let req = Message::read(&mut dec)?;

        let mut resp = Message::new();
        resp.set_id(req.id());
        resp.set_message_type(MessageType::Response);
        resp.set_op_code(OpCode::Query);
        resp.set_response_code(ResponseCode::NoError);

        for q in req.queries() {
            resp.add_query(q.clone());
            if q.query_type() == RecordType::A {
                let mut rec = Record::with(q.name().clone(), RecordType::A, 60);
                rec.set_data(Some(RData::A(A::new(203, 0, 113, 10))));
                resp.add_answer(rec);
            }
        }

        let mut out = Vec::with_capacity(512);
        let mut enc = BinEncoder::new(&mut out);
        resp.emit(&mut enc)?;
        socket.send_to(&out, peer).await?;
    }
}
