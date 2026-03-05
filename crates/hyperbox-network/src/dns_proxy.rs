use std::net::{IpAddr, SocketAddr};

use hickory_proto::{
    op::{Message, MessageType, OpCode, Query, ResponseCode},
    rr::RData,
    serialize::binary::{BinDecodable, BinDecoder, BinEncodable, BinEncoder},
};

use hyperbox_core::NetworkMode;

use crate::NetworkPolicyEvaluator;

#[derive(Debug, Clone)]
pub struct ResolvedIp {
    pub ip: IpAddr,
    pub ttl_secs: u32,
}

#[derive(Debug, Clone)]
pub struct DnsAllowlistProxy {
    pub mode: NetworkMode,
    pub upstream: SocketAddr,
}

impl DnsAllowlistProxy {
    pub fn new(mode: NetworkMode, upstream: SocketAddr) -> Self {
        Self { mode, upstream }
    }

    pub async fn serve(
        &self,
        listen: SocketAddr,
        on_resolve: impl Fn(Vec<ResolvedIp>) + Send + Sync + 'static,
    ) -> anyhow::Result<()> {
        let socket = tokio::net::UdpSocket::bind(listen).await?;
        let on_resolve = std::sync::Arc::new(on_resolve);

        loop {
            let mut buf = vec![0u8; 4096];
            let (len, peer) = socket.recv_from(&mut buf).await?;
            let data = &buf[..len];

            let (response_bytes, resolved) = self.handle_query(data).await?;
            socket.send_to(&response_bytes, peer).await?;
            if !resolved.is_empty() {
                on_resolve(resolved);
            }
        }
    }

    pub async fn handle_query(&self, packet: &[u8]) -> anyhow::Result<(Vec<u8>, Vec<ResolvedIp>)> {
        let mut decoder = BinDecoder::new(packet);
        let request = Message::read(&mut decoder)?;

        let domain = request
            .queries()
            .first()
            .map(Query::name)
            .map(|n| n.to_ascii().trim_end_matches('.').to_string())
            .unwrap_or_default();

        let evaluator = NetworkPolicyEvaluator::new(&self.mode);
        if !evaluator.allows_domain(&self.mode, &domain) {
            let response = denied_response(&request);
            return Ok((encode_message(&response)?, vec![]));
        }

        let upstream_response = forward_udp(self.upstream, packet).await?;
        let mut upstream_decoder = BinDecoder::new(&upstream_response);
        let parsed = Message::read(&mut upstream_decoder)?;

        let resolved = parsed
            .answers()
            .iter()
            .filter_map(|answer| match answer.data() {
                Some(RData::A(ip)) => Some(ResolvedIp {
                    ip: IpAddr::V4((*ip).into()),
                    ttl_secs: answer.ttl(),
                }),
                Some(RData::AAAA(ip)) => Some(ResolvedIp {
                    ip: IpAddr::V6((*ip).into()),
                    ttl_secs: answer.ttl(),
                }),
                _ => None,
            })
            .collect();

        Ok((upstream_response, resolved))
    }
}

async fn forward_udp(upstream: SocketAddr, packet: &[u8]) -> anyhow::Result<Vec<u8>> {
    let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
    socket.send_to(packet, upstream).await?;

    let mut buf = vec![0u8; 4096];
    let (len, _) = socket.recv_from(&mut buf).await?;
    Ok(buf[..len].to_vec())
}

fn denied_response(request: &Message) -> Message {
    let mut response = Message::new();
    response.set_id(request.id());
    response.set_message_type(MessageType::Response);
    response.set_op_code(OpCode::Query);
    response.set_response_code(ResponseCode::NXDomain);
    for query in request.queries() {
        response.add_query(query.clone());
    }
    response
}

fn encode_message(message: &Message) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(512);
    let mut encoder = BinEncoder::new(&mut out);
    message.emit(&mut encoder)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hickory_proto::rr::Name;
    use hickory_proto::rr::RecordType;

    fn build_query(domain: &str) -> Vec<u8> {
        let mut msg = Message::new();
        msg.set_id(42);
        msg.add_query(Query::query(
            Name::from_ascii(domain).expect("domain name"),
            RecordType::A,
        ));
        encode_message(&msg).expect("encode query")
    }

    #[tokio::test]
    async fn blocks_non_allowlisted_domain() {
        let proxy = DnsAllowlistProxy::new(
            NetworkMode::Allowlist(vec!["api.openai.com".to_string()]),
            "1.1.1.1:53".parse().expect("upstream socket"),
        );

        let query = build_query("example.com");
        let (response, resolved) = proxy.handle_query(&query).await.expect("handle query");
        assert!(resolved.is_empty());

        let mut decoder = BinDecoder::new(&response);
        let parsed = Message::read(&mut decoder).expect("parse response");
        assert_eq!(parsed.response_code(), ResponseCode::NXDomain);
    }
}
