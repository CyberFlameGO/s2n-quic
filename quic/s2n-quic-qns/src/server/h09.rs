// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

use crate::{
    file::{abs_path, File},
    server::interop::MyConnectionContext,
    Result,
};
use bytes::Bytes;
use futures::StreamExt;
use s2n_quic::{
    stream::{BidirectionalStream, ReceiveStream},
    Connection,
};
use std::{convert::TryInto, path::Path, sync::Arc, time::Duration};
use tokio::time::timeout;
use tracing::debug;

pub(crate) async fn handle_connection(mut connection: Connection, www_dir: Arc<Path>) {
    loop {
        match connection.accept_bidirectional_stream().await {
            Ok(Some(stream)) => {
                let _ = connection.query_event_context_mut(|context: &mut MyConnectionContext| {
                    context.stream_requests += 1
                });

                let www_dir = www_dir.clone();
                // spawn a task per stream
                tokio::spawn(async move {
                    if let Err(err) = handle_stream(stream, www_dir).await {
                        eprintln!("Stream error: {:?}", err)
                    }
                });
            }
            Ok(None) => {
                // the connection was closed without an error
                let context = connection
                    .query_event_context(|context: &MyConnectionContext| *context)
                    .expect("query should execute");
                debug!("Final stats: {:?}", context);
                return;
            }
            Err(err) => {
                eprintln!("error while accepting stream: {}", err);
                let context = connection
                    .query_event_context(|context: &MyConnectionContext| *context)
                    .expect("query should execute");
                debug!("Final stats: {:?}", context);
                return;
            }
        }
    }
}

async fn handle_stream(stream: BidirectionalStream, www_dir: Arc<Path>) -> Result<()> {
    let (rx_stream, mut tx_stream) = stream.split();
    let path = read_request(rx_stream).await?;
    let abs_path = abs_path(&path, &www_dir);
    let mut file = File::open(&abs_path).await?;
    loop {
        match timeout(Duration::from_secs(1), file.next()).await {
            Ok(Some(Ok(chunk))) => {
                let len = chunk.len();
                debug!(
                    "{:?} bytes ready to send on Stream({:?})",
                    len,
                    tx_stream.id()
                );
                tx_stream.send(chunk).await?;
                debug!("{:?} bytes sent on Stream({:?})", len, tx_stream.id());
            }
            Ok(Some(Err(err))) => {
                eprintln!("error opening {:?}", abs_path);
                tx_stream.reset(1u32.try_into()?)?;
                return Err(err.into());
            }
            Ok(None) => {
                tx_stream.finish()?;
                return Ok(());
            }
            Err(_) => {
                eprintln!("timeout opening {:?}", abs_path);
            }
        }
    }
}

async fn read_request(mut stream: ReceiveStream) -> Result<String> {
    let mut path = String::new();
    let mut chunks = vec![Bytes::new(), Bytes::new()];
    let mut total_chunks = 0;
    loop {
        // grow the chunks
        if chunks.len() == total_chunks {
            chunks.push(Bytes::new());
        }
        let (consumed, is_open) = stream.receive_vectored(&mut chunks[total_chunks..]).await?;
        total_chunks += consumed;
        if parse_h09_request(&chunks[..total_chunks], &mut path, is_open)? {
            return Ok(path);
        }
    }
}

fn parse_h09_request(chunks: &[Bytes], path: &mut String, is_open: bool) -> Result<bool> {
    let mut bytes = chunks.iter().flat_map(|chunk| chunk.iter().cloned());

    macro_rules! expect {
        ($char:literal) => {
            match bytes.next() {
                Some($char) => {}
                None if is_open => return Ok(false),
                _ => return Err("invalid request".into()),
            }
        };
    }

    expect!(b'G');
    expect!(b'E');
    expect!(b'T');
    expect!(b' ');
    expect!(b'/');

    // reset the copied path in case this isn't the first time a path is being parsed
    path.clear();

    loop {
        match bytes.next() {
            Some(c @ b'0'..=b'9') => path.push(c as char),
            Some(c @ b'a'..=b'z') => path.push(c as char),
            Some(c @ b'A'..=b'Z') => path.push(c as char),
            Some(b'.') => path.push('.'),
            Some(b'/') => path.push('/'),
            Some(b'-') => path.push('-'),
            Some(b'\n' | b'\r') => return Ok(true),
            Some(c) => return Err(format!("invalid request {}", c as char).into()),
            None => return Ok(!is_open),
        }
    }
}

#[test]
fn parse_h09_request_test() {
    macro_rules! test {
        ([$($chunk:expr),* $(,)?], $expected:pat) => {{
            let chunks = [$(Bytes::from_static($chunk.as_bytes())),*];
            let mut path = String::new();

            for idx in 0..chunks.len() {
                let _ = parse_h09_request(&chunks[..idx], &mut path, true);
            }

            let result = parse_h09_request(&chunks, &mut path, false);
            let result = result.map(|has_request| if has_request { Some(path) } else { None });
            let result = result.as_ref().map(|v| v.as_deref());

            assert!(matches!(result, $expected), "{:?}", result);
        }}
    }

    test!([], Err(_));
    test!(["GET /"], Ok(Some("")));
    test!(["GET /abc"], Ok(Some("abc")));
    test!(["GET /abc/123"], Ok(Some("abc/123")));
    test!(["GET /CAPS/lower"], Ok(Some("CAPS/lower")));
    test!(["GET /abc\rextra stuff"], Ok(Some("abc")));
    test!(
        ["G", "E", "T", " ", "/", "t", "E", "s", "T"],
        Ok(Some("tEsT"))
    );
}
