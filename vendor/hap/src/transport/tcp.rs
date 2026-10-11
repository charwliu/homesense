use aead::{array::Array, AeadInPlace, KeyInit};
use byteorder::{ByteOrder, LittleEndian};
use bytes::{Buf, BytesMut};
use chacha20poly1305::{ChaCha20Poly1305, Nonce, Tag};
use futures::{
    channel::{
        mpsc::{self, Receiver, Sender},
        oneshot,
    },
    io::Error,
    Stream,
};
use log::debug;
use std::{
    cmp::min,
    future::Future,
    io::{self, ErrorKind},
    pin::Pin,
    sync::{Arc, Mutex, RwLock},
    task::{Context, Poll, Waker},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::TcpStream,
};
use uuid::Uuid;

use crate::Result;

#[derive(Debug)]
pub struct StreamWrapper {
    incoming_receiver: Receiver<Vec<u8>>,
    outgoing_sender: OutputSender,
    incoming_waker: Arc<Mutex<Option<Waker>>>,
    outgoing_waker: Arc<Mutex<Option<Waker>>>,
    incoming_buf: BytesMut,
}

impl StreamWrapper {
    pub fn new(
        incoming_receiver: Receiver<Vec<u8>>,
        outgoing_sender: OutputSender,
        incoming_waker: Arc<Mutex<Option<Waker>>>,
        outgoing_waker: Arc<Mutex<Option<Waker>>>,
    ) -> StreamWrapper {
        StreamWrapper {
            incoming_receiver,
            outgoing_sender,
            incoming_waker,
            outgoing_waker,
            incoming_buf: BytesMut::new(),
        }
    }

    fn poll_receiver(&mut self, cx: &mut Context) -> Poll<usize> {
        debug!("polling incoming TCP stream receiver");

        match Stream::poll_next(Pin::new(&mut self.incoming_receiver), cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(incoming)) => {
                let r_len = incoming.len();
                self.incoming_buf.extend_from_slice(&incoming);

                debug!("received {} Bytes on incoming TCP stream receiver", &r_len);

                Poll::Ready(r_len)
            }
            Poll::Ready(None) => {
                debug!("received 0 Bytes on incoming TCP stream receiver");
                Poll::Ready(0)
            }
        }
    }
}

impl AsyncRead for StreamWrapper {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &mut ReadBuf,
    ) -> Poll<std::result::Result<(), io::Error>> {
        let stream_wrapper = Pin::into_inner(self);

        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        let available = if stream_wrapper.incoming_buf.is_empty() {
            stream_wrapper.poll_receiver(cx)
        } else {
            Poll::Ready(stream_wrapper.incoming_buf.len())
        };
        match available {
            Poll::Pending => Poll::Pending,
            Poll::Ready(_r_len) => {
                let r_len = min(buf.remaining(), stream_wrapper.incoming_buf.len());
                buf.put_slice(&stream_wrapper.incoming_buf[..r_len]);
                stream_wrapper.incoming_buf.advance(r_len);

                if let Some(waker) = stream_wrapper
                    .outgoing_waker
                    .lock()
                    .expect("accessing outgoing_waker")
                    .take()
                {
                    waker.wake()
                }
                if let Some(waker) = stream_wrapper
                    .incoming_waker
                    .lock()
                    .expect("accessing incoming_waker")
                    .take()
                {
                    waker.wake()
                }

                Poll::Ready(Ok(()))
            }
        }
    }
}

impl AsyncWrite for StreamWrapper {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context,
        buf: &[u8],
    ) -> Poll<std::result::Result<usize, io::Error>> {
        let stream_wrapper = Pin::into_inner(self);

        debug!("writing {} Bytes to outgoing TCP stream sender", buf.len());

        stream_wrapper
            .outgoing_sender
            .try_send(buf.to_vec())
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "couldn't write"))?;

        if let Some(waker) = stream_wrapper
            .outgoing_waker
            .lock()
            .expect("accessing outgoing_waker")
            .take()
        {
            waker.wake()
        }
        if let Some(waker) = stream_wrapper
            .incoming_waker
            .lock()
            .expect("accessing incoming_waker")
            .take()
        {
            waker.wake()
        }

        let w_len = buf.len();

        debug!("wrote {} Bytes to outgoing TCP stream sender", &w_len);

        Poll::Ready(Ok(w_len))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        let sender = &self.get_mut().outgoing_sender;
        sender.drain_waker.register(cx.waker());
        if sender.aborted.load(std::sync::atomic::Ordering::Acquire) {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }
        if sender.used.load(std::sync::atomic::Ordering::Acquire) == 0 {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        self.poll_flush(cx)
    }
}

#[derive(Debug)]
pub struct Session {
    pub controller_id: Uuid,
    pub shared_secret: [u8; 32],
}

#[derive(Debug)]
pub struct EncryptedStream {
    stream: TcpStream,
    incoming_sender: Sender<Vec<u8>>,
    outgoing_receiver: Receiver<QueuedOutput>,
    output: OutputSender,
    outgoing_pending: Option<(QueuedOutput, usize)>,
    record_writer: RecordWriter,
    incoming_waker: Arc<Mutex<Option<Waker>>>,
    outgoing_waker: Arc<Mutex<Option<Waker>>>,
    session_receiver: oneshot::Receiver<Session>,
    pub controller_id: Arc<RwLock<Option<Uuid>>>,
    shared_secret: Option<[u8; 32]>,
    pending_controller: Option<Uuid>,
    encrypted_peer_confirmed: bool,
    decrypt_count: u64,
    encrypt_count: u64,
    record_reader: RecordReader,
}

impl EncryptedStream {
    pub fn new(
        stream: TcpStream,
    ) -> (
        EncryptedStream,
        Receiver<Vec<u8>>,
        OutputSender,
        oneshot::Sender<Session>,
        Arc<Mutex<Option<Waker>>>,
        Arc<Mutex<Option<Waker>>>,
    ) {
        let (sender, receiver) = oneshot::channel();
        let (incoming_sender, incoming_receiver) = mpsc::channel(63);
        let (outgoing_sender, outgoing_receiver) = OutputSender::channel();
        let incoming_waker = Arc::new(Mutex::new(None));
        let outgoing_waker = Arc::new(Mutex::new(None));

        (
            EncryptedStream {
                stream,
                incoming_sender,
                outgoing_receiver,
                output: outgoing_sender.clone(),
                outgoing_pending: None,
                record_writer: RecordWriter::default(),
                incoming_waker: incoming_waker.clone(),
                outgoing_waker: outgoing_waker.clone(),
                session_receiver: receiver,
                controller_id: Arc::new(RwLock::new(None)),
                shared_secret: None,
                pending_controller: None,
                encrypted_peer_confirmed: false,
                decrypt_count: 0,
                encrypt_count: 0,
                record_reader: RecordReader::default(),
            },
            incoming_receiver,
            outgoing_sender,
            sender,
            incoming_waker,
            outgoing_waker,
        )
    }

    fn poll_outgoing(
        self: Pin<&mut Self>,
        cx: &mut Context,
    ) -> Poll<std::result::Result<(), io::Error>> {
        let encrypted_stream = Pin::into_inner(self);
        loop {
            if encrypted_stream.outgoing_pending.is_none() {
                match Stream::poll_next(Pin::new(&mut encrypted_stream.outgoing_receiver), cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(None) => return Poll::Ready(Ok(())),
                    Poll::Ready(Some(data)) => encrypted_stream.outgoing_pending = Some((data, 0)),
                }
            }
            let (data, mut offset) = encrypted_stream.outgoing_pending.take().unwrap();
            if offset == data.len() {
                continue;
            }
            match AsyncWrite::poll_write(Pin::new(&mut *encrypted_stream), cx, &data[offset..]) {
                Poll::Pending => {
                    encrypted_stream.outgoing_pending = Some((data, offset));
                    return Poll::Pending;
                }
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Ready(Ok(0)) => {
                    return Poll::Ready(Err(io::Error::new(
                        ErrorKind::WriteZero,
                        "HAP socket write made no progress",
                    )))
                }
                Poll::Ready(Ok(written)) => {
                    offset += written;
                    if offset < data.len() {
                        encrypted_stream.outgoing_pending = Some((data, offset));
                    }
                }
            }
        }
    }

    fn poll_incoming(
        self: Pin<&mut Self>,
        cx: &mut Context,
    ) -> Poll<std::result::Result<(), io::Error>> {
        let encrypted_stream = Pin::into_inner(self);

        let mut data_inner = [0; 1536];
        let mut data = ReadBuf::new(&mut data_inner);

        loop {
            match AsyncRead::poll_read(Pin::new(encrypted_stream), cx, &mut data) {
                Poll::Pending => {
                    *encrypted_stream
                        .incoming_waker
                        .lock()
                        .expect("setting incoming_waker") = Some(cx.waker().clone());
                    return Poll::Pending;
                }
                Poll::Ready(Err(e)) => match e.kind() {
                    ErrorKind::WouldBlock => {
                        *encrypted_stream
                            .incoming_waker
                            .lock()
                            .expect("setting incoming_waker") = Some(cx.waker().clone());
                        return Poll::Pending;
                    }
                    _ => {
                        return Poll::Ready(Err(e));
                    }
                },
                Poll::Ready(Ok(())) => {
                    let data_filled = data.filled();

                    if data_filled.len() == 0 {
                        return Poll::Ready(Ok(()));
                    }

                    encrypted_stream
                        .incoming_sender
                        .try_send(data_filled.to_vec())
                        .map_err(|_| {
                            io::Error::new(io::ErrorKind::Other, "couldn't send incoming data")
                        })?;

                    data.clear();
                }
            }
        }
    }
}

impl Future for EncryptedStream {
    type Output = std::result::Result<(), io::Error>;

    #[allow(unused_must_use)]
    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        let encrypted_stream = Pin::into_inner(self);
        encrypted_stream.output.abort_waker.register(cx.waker());
        if encrypted_stream
            .output
            .aborted
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Poll::Ready(Err(io::Error::new(
                ErrorKind::ConnectionAborted,
                "HAP output capacity exceeded or closed",
            )));
        }
        EncryptedStream::poll_outgoing(Pin::new(encrypted_stream), cx)?;
        EncryptedStream::poll_incoming(Pin::new(encrypted_stream), cx)
    }
}

impl AsyncRead for EncryptedStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &mut ReadBuf,
    ) -> Poll<std::result::Result<(), io::Error>> {
        let encrypted_stream = Pin::into_inner(self);

        if encrypted_stream.shared_secret.is_none() {
            match encrypted_stream.session_receiver.try_recv() {
                Ok(Some(session)) => {
                    // Pair Verify's final response is still plaintext. A verified
                    // peer must receive it before sending its first encrypted
                    // request; receiving the session alone is not a wire boundary.
                    encrypted_stream.pending_controller = Some(session.controller_id);
                    encrypted_stream.shared_secret = Some(session.shared_secret);
                }
                _ => {
                    return AsyncRead::poll_read(Pin::new(&mut encrypted_stream.stream), cx, buf);
                }
            }
        }

        let before = buf.filled().len();
        let result = encrypted_stream.record_reader.poll_read(
            &mut encrypted_stream.stream,
            cx,
            buf,
            &encrypted_stream.shared_secret.unwrap(),
            &mut encrypted_stream.decrypt_count,
        );
        if matches!(result, Poll::Ready(Ok(()))) && buf.filled().len() > before {
            encrypted_stream.encrypted_peer_confirmed = true;
            if let Some(controller) = encrypted_stream.pending_controller.take() {
                *encrypted_stream
                    .controller_id
                    .write()
                    .expect("setting controller_id") = Some(controller);
            }
        }
        result
    }
}

impl AsyncWrite for EncryptedStream {
    #[allow(unused_must_use)]
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &[u8],
    ) -> Poll<std::result::Result<usize, Error>> {
        let encrypted_stream = Pin::into_inner(self);

        if let Some(shared_secret) = encrypted_stream
            .shared_secret
            .filter(|_| encrypted_stream.encrypted_peer_confirmed)
        {
            encrypted_stream.record_writer.poll_write(
                &mut encrypted_stream.stream,
                cx,
                buf,
                &shared_secret,
                &mut encrypted_stream.encrypt_count,
            )
        } else {
            AsyncWrite::poll_write(Pin::new(&mut encrypted_stream.stream), cx, buf)
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context) -> Poll<std::result::Result<(), Error>> {
        let encrypted_stream = Pin::into_inner(self);
        AsyncWrite::poll_flush(Pin::new(&mut encrypted_stream.stream), cx)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _cx: &mut Context,
    ) -> Poll<std::result::Result<(), Error>> {
        Poll::Ready(Ok(()))
    }
}

fn decrypt_chunk(
    shared_secret: &[u8; 32],
    aad: &[u8],
    data: &[u8],
    auth_tag: &[u8],
    count: &mut u64,
) -> Result<Vec<u8>> {
    let read_key = compute_read_key(shared_secret)?;
    let aead = ChaCha20Poly1305::new(Array::from_slice(&read_key));

    let mut nonce = vec![0; 4];
    let mut suffix = vec![0; 8];
    LittleEndian::write_u64(&mut suffix, *count);
    nonce.extend(suffix);
    *count = count.checked_add(1).ok_or(crate::Error::Aead)?;

    let mut buffer = Vec::new();
    buffer.extend_from_slice(data);
    aead.decrypt_in_place_detached(
        Nonce::from_slice(&nonce),
        aad,
        &mut buffer,
        Tag::from_slice(&auth_tag),
    )?;

    Ok(buffer)
}

fn encrypt_chunk(
    shared_secret: &[u8; 32],
    data: &[u8],
    count: &mut u64,
) -> Result<([u8; 2], Vec<u8>, [u8; 16])> {
    let write_key = compute_write_key(shared_secret)?;
    let aead = ChaCha20Poly1305::new(Array::from_slice(&write_key));

    let mut nonce = vec![0; 4];
    let mut suffix = vec![0; 8];
    LittleEndian::write_u64(&mut suffix, *count);
    nonce.extend(suffix);
    *count = count.checked_add(1).ok_or(crate::Error::Aead)?;

    let mut aad = [0; 2];
    LittleEndian::write_u16(&mut aad, data.len() as u16);

    let mut buffer = Vec::new();
    buffer.extend_from_slice(data);
    let auth_tag = aead.encrypt_in_place_detached(Nonce::from_slice(&nonce), &aad, &mut buffer)?;

    Ok((aad, buffer, auth_tag.into()))
}

fn compute_read_key(shared_secret: &[u8; 32]) -> Result<[u8; 32]> {
    compute_key(shared_secret, b"Control-Write-Encryption-Key")
}

fn compute_write_key(shared_secret: &[u8; 32]) -> Result<[u8; 32]> {
    compute_key(shared_secret, b"Control-Read-Encryption-Key")
}

fn compute_key(shared_secret: &[u8; 32], info: &[u8]) -> Result<[u8; 32]> {
    super::hkdf_extract_and_expand(b"Control-Salt", shared_secret, info)
}

/// Holds one encoded record across socket Pending/short writes. Plaintext is
/// acknowledged only after that record is completely written; retries neither
/// encrypt again nor consume another nonce. At most 1042 wire bytes are retained.
#[derive(Debug, Default)]
struct RecordWriter {
    pending: Option<(Vec<u8>, usize, usize)>,
}
impl RecordWriter {
    fn poll_write<W: AsyncWrite + Unpin>(
        &mut self,
        stream: &mut W,
        cx: &mut Context<'_>,
        plaintext: &[u8],
        secret: &[u8; 32],
        counter: &mut u64,
    ) -> Poll<io::Result<usize>> {
        if self.pending.is_none() {
            if plaintext.is_empty() {
                return Poll::Ready(Ok(0));
            }
            let len = plaintext.len().min(1024);
            let (aad, data, tag) = encrypt_chunk(secret, &plaintext[..len], counter)
                .map_err(|_| io::Error::new(ErrorKind::Other, "HAP encryption failed"))?;
            self.pending = Some(([&aad[..], &data[..], &tag[..]].concat(), 0, len));
        }
        let (record, offset, accepted) = self.pending.as_mut().unwrap();
        while *offset < record.len() {
            match Pin::new(&mut *stream).poll_write(cx, &record[*offset..]) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Ready(Ok(0)) => {
                    return Poll::Ready(Err(io::Error::new(
                        ErrorKind::WriteZero,
                        "HAP record write made no progress",
                    )))
                }
                Poll::Ready(Ok(written)) => *offset += written,
            }
        }
        let accepted = *accepted;
        self.pending = None;
        Poll::Ready(Ok(accepted))
    }
}

#[cfg(test)]
mod record_tests {
    use super::*;
    #[derive(Default)]
    struct ShortSocket {
        bytes: Vec<u8>,
        pending: bool,
        zero: bool,
    }
    impl AsyncWrite for ShortSocket {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            if self.pending {
                self.pending = false;
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            if self.zero {
                return Poll::Ready(Ok(0));
            }
            let n = bytes.len().min(7);
            self.bytes.extend_from_slice(&bytes[..n]);
            self.pending = true;
            Poll::Ready(Ok(n))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }
    #[tokio::test]
    async fn encrypted_records_survive_repeated_pending_and_short_socket_writes() {
        let secret = [42; 32];
        let plaintext: Vec<u8> = (0..2500).map(|i| (i % 251) as u8).collect();
        let mut writer = RecordWriter::default();
        let mut socket = ShortSocket::default();
        let mut counter = 0;
        let mut offset = 0;
        while offset < plaintext.len() {
            let accepted = futures::future::poll_fn(|cx| {
                writer.poll_write(&mut socket, cx, &plaintext[offset..], &secret, &mut counter)
            })
            .await
            .unwrap();
            assert!(accepted > 0 && accepted <= 1024);
            offset += accepted;
        }
        assert_eq!(counter, 3);
        assert!(writer.pending.is_none());
        // Independent record parsing/decryption checks lengths, byte order and
        // nonce sequence, not merely equivalence to this writer's own encoder.
        let cipher = ChaCha20Poly1305::new(Array::from_slice(
            &compute_write_key(&secret).unwrap(),
        ));
        let mut position = 0;
        let mut decoded = Vec::new();
        let mut nonce_count = 0u64;
        while position < socket.bytes.len() {
            let aad = &socket.bytes[position..position + 2];
            let len = LittleEndian::read_u16(aad) as usize;
            assert!(len > 0 && len <= 1024);
            let mut value = socket.bytes[position + 2..position + 2 + len].to_vec();
            let tag = &socket.bytes[position + 2 + len..position + 2 + len + 16];
            let mut nonce = [0u8; 12];
            LittleEndian::write_u64(&mut nonce[4..], nonce_count);
            cipher
                .decrypt_in_place_detached(
                    Nonce::from_slice(&nonce),
                    aad,
                    &mut value,
                    Tag::from_slice(tag),
                )
                .unwrap();
            decoded.extend(value);
            position += len + 18;
            nonce_count += 1;
        }
        assert_eq!(decoded, plaintext);
        assert_eq!(nonce_count, 3);
    }
    #[tokio::test]
    async fn zero_socket_write_and_nonce_exhaustion_never_report_plaintext_success() {
        let mut writer = RecordWriter::default();
        let mut socket = ShortSocket {
            zero: true,
            ..Default::default()
        };
        let mut count = 0;
        let error = futures::future::poll_fn(|cx| {
            writer.poll_write(&mut socket, cx, b"test", &[1; 32], &mut count)
        })
        .await
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::WriteZero);
        assert!(socket.bytes.is_empty());
        assert_eq!(count, 1);
        let mut writer = RecordWriter::default();
        let mut count = u64::MAX;
        let mut socket = ShortSocket::default();
        assert!(futures::future::poll_fn(|cx| writer.poll_write(
            &mut socket,
            cx,
            b"test",
            &[1; 32],
            &mut count
        ))
        .await
        .is_err());
        assert!(socket.bytes.is_empty());
        assert_eq!(count, u64::MAX);
    }
}

const OUTPUT_BYTES: usize = 1024 * 1024;
const OUTPUT_MESSAGE_BYTES: usize = 256 * 1024;
#[derive(Debug)]
pub(crate) struct QueuedOutput {
    data: Vec<u8>,
    used: Arc<std::sync::atomic::AtomicUsize>,
    drain_waker: Arc<futures::task::AtomicWaker>,
}
impl std::ops::Deref for QueuedOutput {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.data
    }
}
impl Drop for QueuedOutput {
    fn drop(&mut self) {
        self.used
            .fetch_sub(self.data.len(), std::sync::atomic::Ordering::AcqRel);
        self.drain_waker.wake();
    }
}
#[derive(Clone, Debug)]
pub struct OutputSender {
    // Clones share ONE channel sender: no extra reserved slots per event clone.
    sender: Arc<Mutex<mpsc::Sender<QueuedOutput>>>,
    used: Arc<std::sync::atomic::AtomicUsize>,
    drain_waker: Arc<futures::task::AtomicWaker>,
    aborted: Arc<std::sync::atomic::AtomicBool>,
    abort_waker: Arc<futures::task::AtomicWaker>,
}
impl OutputSender {
    pub(crate) fn channel() -> (Self, Receiver<QueuedOutput>) {
        // futures MPSC has one extra reserved slot for its single sender.
        let (sender, receiver) = mpsc::channel(63);
        (
            Self {
                sender: Arc::new(Mutex::new(sender)),
                used: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                drain_waker: Arc::new(futures::task::AtomicWaker::new()),
                aborted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                abort_waker: Arc::new(futures::task::AtomicWaker::new()),
            },
            receiver,
        )
    }
    pub(crate) fn abort(&self) {
        self.aborted
            .store(true, std::sync::atomic::Ordering::Release);
        self.abort_waker.wake();
        self.drain_waker.wake();
    }
    pub fn try_send(&self, data: Vec<u8>) -> io::Result<()> {
        use std::sync::atomic::Ordering;
        if self.aborted.load(Ordering::Acquire) {
            return Err(io::Error::new(
                ErrorKind::ConnectionAborted,
                "HAP output closed",
            ));
        }
        if data.len() > OUTPUT_MESSAGE_BYTES
            || self
                .used
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                    used.checked_add(data.len())
                        .filter(|size| *size <= OUTPUT_BYTES)
                })
                .is_err()
        {
            self.abort();
            return Err(io::Error::new(
                ErrorKind::OutOfMemory,
                "HAP output byte capacity",
            ));
        }
        let packet = QueuedOutput {
            data,
            used: self.used.clone(),
            drain_waker: self.drain_waker.clone(),
        };
        let sent = self
            .sender
            .lock()
            .map_err(|_| ())
            .and_then(|mut sender| sender.try_send(packet).map_err(|_| ()));
        if sent.is_err() {
            self.abort();
            return Err(io::Error::new(
                ErrorKind::ConnectionAborted,
                "HAP output queue full or closed",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod output_capacity_tests {
    use super::*;
    use futures::StreamExt;
    use std::sync::atomic::Ordering;
    #[test]
    fn repeated_sender_clones_cannot_expand_capacity_and_overflow_is_terminal() {
        let (sender, receiver) = OutputSender::channel();
        struct WakeCount(std::sync::atomic::AtomicUsize);
        impl futures::task::ArcWake for WakeCount {
            fn wake_by_ref(value: &Arc<Self>) {
                value.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let wakes = Arc::new(WakeCount(std::sync::atomic::AtomicUsize::new(0)));
        sender
            .abort_waker
            .register(&futures::task::waker_ref(&wakes));
        let mut accepted = 0;
        for _ in 0..1000 {
            if sender.clone().try_send(vec![1]).is_err() {
                break;
            }
            accepted += 1;
        }
        assert!(accepted > 0 && accepted <= 64);
        assert!(wakes.0.load(Ordering::SeqCst) > 0);
        assert!(sender.aborted.load(Ordering::Acquire));
        assert!(sender.try_send(vec![2]).is_err());
        drop(receiver);
        assert_eq!(sender.used.load(Ordering::Acquire), 0);
    }
    #[tokio::test]
    async fn byte_budget_includes_dequeued_pending_packets_and_releases_on_drop() {
        let (sender, mut receiver) = OutputSender::channel();
        for _ in 0..4 {
            sender.try_send(vec![7; OUTPUT_MESSAGE_BYTES]).unwrap();
        }
        let pending = receiver.next().await.unwrap();
        assert_eq!(sender.used.load(Ordering::Acquire), OUTPUT_BYTES);
        drop(pending);
        assert_eq!(
            sender.used.load(Ordering::Acquire),
            OUTPUT_BYTES - OUTPUT_MESSAGE_BYTES
        );
        sender.try_send(vec![8; OUTPUT_MESSAGE_BYTES]).unwrap();
        assert!(sender.try_send(vec![9]).is_err());
        drop(receiver);
        assert_eq!(sender.used.load(Ordering::Acquire), 0);
    }
    #[test]
    fn oversized_message_and_closed_receiver_fail_without_leaking_budget() {
        let (sender, receiver) = OutputSender::channel();
        assert!(sender.try_send(vec![0; OUTPUT_MESSAGE_BYTES + 1]).is_err());
        assert_eq!(sender.used.load(Ordering::Acquire), 0);
        drop(receiver);
        let (sender, receiver) = OutputSender::channel();
        drop(receiver);
        assert!(sender.try_send(vec![0; 8]).is_err());
        assert_eq!(sender.used.load(Ordering::Acquire), 0);
    }
}

/// Bounded record parser. Complete records may arrive together or split at any
/// byte; unread plaintext is drained before polling the socket again.
#[derive(Debug, Default)]
struct RecordReader {
    encrypted: BytesMut,
    plaintext: BytesMut,
    failed: bool,
}
impl RecordReader {
    fn poll_read<R: AsyncRead + Unpin>(
        &mut self,
        stream: &mut R,
        cx: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
        secret: &[u8; 32],
        counter: &mut u64,
    ) -> Poll<io::Result<()>> {
        if self.failed {
            return Poll::Ready(Err(io::Error::new(
                ErrorKind::InvalidData,
                "HAP record reader failed",
            )));
        }
        if output.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        for _ in 0..32 {
            if !self.plaintext.is_empty() {
                let count = output.remaining().min(self.plaintext.len());
                output.put_slice(&self.plaintext[..count]);
                self.plaintext.advance(count);
                return Poll::Ready(Ok(()));
            }
            if self.encrypted.len() >= 2 {
                let len = LittleEndian::read_u16(&self.encrypted[..2]) as usize;
                if len > 1024 {
                    self.failed = true;
                    return Poll::Ready(Err(io::Error::new(
                        ErrorKind::InvalidData,
                        "HAP record too large",
                    )));
                }
                if self.encrypted.len() >= len + 18 {
                    let decoded = decrypt_chunk(
                        secret,
                        &self.encrypted[..2],
                        &self.encrypted[2..len + 2],
                        &self.encrypted[len + 2..len + 18],
                        counter,
                    );
                    match decoded {
                        Ok(value) => {
                            self.encrypted.advance(len + 18);
                            self.plaintext.extend_from_slice(&value);
                            continue;
                        }
                        Err(_) => {
                            self.failed = true;
                            return Poll::Ready(Err(io::Error::new(
                                ErrorKind::InvalidData,
                                "HAP record authentication failed",
                            )));
                        }
                    }
                }
            }
            let mut chunk = [0u8; 1042];
            let mut input = ReadBuf::new(&mut chunk);
            match Pin::new(&mut *stream).poll_read(cx, &mut input) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => {
                    self.failed = true;
                    return Poll::Ready(Err(error));
                }
                Poll::Ready(Ok(())) => {
                    if input.filled().is_empty() {
                        if self.encrypted.is_empty() {
                            return Poll::Ready(Ok(()));
                        }
                        self.failed = true;
                        return Poll::Ready(Err(io::Error::new(
                            ErrorKind::UnexpectedEof,
                            "truncated HAP record",
                        )));
                    }
                    self.encrypted.extend_from_slice(input.filled());
                }
            }
        }
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

#[cfg(test)]
mod read_record_tests {
    use super::*;
    struct Chunks {
        data: Vec<u8>,
        offset: usize,
        chunk: usize,
        pending: bool,
    }
    impl AsyncRead for Chunks {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            out: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            if self.pending {
                self.pending = false;
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            let n = (self.data.len() - self.offset)
                .min(self.chunk)
                .min(out.remaining());
            out.put_slice(&self.data[self.offset..self.offset + n]);
            self.offset += n;
            self.pending = true;
            Poll::Ready(Ok(()))
        }
    }
    fn encoded(secret: &[u8; 32], messages: &[Vec<u8>]) -> Vec<u8> {
        let cipher =
            ChaCha20Poly1305::new(Array::from_slice(&compute_read_key(secret).unwrap()));
        let mut wire = Vec::new();
        for (count, message) in messages.iter().enumerate() {
            let mut aad = [0; 2];
            LittleEndian::write_u16(&mut aad, message.len() as u16);
            let mut nonce = [0; 12];
            LittleEndian::write_u64(&mut nonce[4..], count as u64);
            let mut value = message.clone();
            let tag = cipher
                .encrypt_in_place_detached(Nonce::from_slice(&nonce), &aad, &mut value)
                .unwrap();
            wire.extend(aad);
            wire.extend(value);
            wire.extend(tag);
        }
        wire
    }
    #[tokio::test]
    async fn fragmented_and_coalesced_records_drain_through_small_prefilled_reads() {
        let secret = [5; 32];
        let messages = vec![
            vec![1],
            (0..1024).map(|v| (v % 251) as u8).collect(),
            vec![3; 37],
        ];
        let wire = encoded(&secret, &messages);
        let expected: Vec<u8> = messages.concat();
        for chunk in [1, 2, 7, 1042, 4096] {
            let mut input = Chunks {
                data: wire.clone(),
                offset: 0,
                chunk,
                pending: true,
            };
            let mut reader = RecordReader::default();
            let mut count = 0;
            let mut output = Vec::new();
            loop {
                let mut bytes = [0u8; 19];
                let mut buffer = ReadBuf::new(&mut bytes);
                buffer.put_slice(&[99]);
                futures::future::poll_fn(|cx| {
                    reader.poll_read(&mut input, cx, &mut buffer, &secret, &mut count)
                })
                .await
                .unwrap();
                assert_eq!(buffer.filled()[0], 99);
                assert!(reader.encrypted.len() <= 2083);
                assert!(reader.plaintext.len() <= 1024);
                if buffer.filled().len() == 1 {
                    break;
                }
                output.extend_from_slice(&buffer.filled()[1..]);
            }
            assert_eq!(output, expected);
            assert_eq!(count, 3);
        }
    }
    #[tokio::test]
    async fn malformed_truncated_and_bad_tag_records_fail_without_plaintext() {
        let secret = [6; 32];
        let wire = encoded(&secret, &[vec![42; 23]]);
        let mut cases = Vec::new();
        for end in 1..wire.len() {
            cases.push((wire[..end].to_vec(), ErrorKind::UnexpectedEof));
        }
        let mut bad = wire.clone();
        *bad.last_mut().unwrap() ^= 1;
        cases.push((bad, ErrorKind::InvalidData));
        cases.push((vec![1, 4], ErrorKind::InvalidData));
        for (data, kind) in cases {
            let mut input = Chunks {
                data,
                offset: 0,
                chunk: 7,
                pending: false,
            };
            let mut reader = RecordReader::default();
            let mut count = 0;
            let mut bytes = [0u8; 64];
            let mut out = ReadBuf::new(&mut bytes);
            let error = futures::future::poll_fn(|cx| {
                reader.poll_read(&mut input, cx, &mut out, &secret, &mut count)
            })
            .await
            .unwrap_err();
            assert_eq!(error.kind(), kind);
            assert!(out.filled().is_empty());
            let before = count;
            assert!(futures::future::poll_fn(
                |cx| reader.poll_read(&mut input, cx, &mut out, &secret, &mut count)
            )
            .await
            .is_err());
            assert_eq!(count, before);
        }
    }
    #[tokio::test]
    async fn wrapper_drains_buffer_without_waiting_for_another_packet() {
        let (mut sender, receiver) = mpsc::channel(63);
        sender.try_send(b"abcdef".to_vec()).unwrap();
        let (out, _) = OutputSender::channel();
        let mut wrapper = StreamWrapper::new(
            receiver,
            out,
            Arc::new(Mutex::new(None)),
            Arc::new(Mutex::new(None)),
        );
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        let mut bytes = [0; 4];
        let mut first = ReadBuf::new(&mut bytes);
        first.put_slice(b"!");
        assert!(Pin::new(&mut wrapper)
            .poll_read(&mut cx, &mut first)
            .is_ready());
        assert_eq!(first.filled(), b"!abc");
        let mut bytes = [0; 3];
        let mut second = ReadBuf::new(&mut bytes);
        assert!(Pin::new(&mut wrapper)
            .poll_read(&mut cx, &mut second)
            .is_ready());
        assert_eq!(second.filled(), b"def");
        let mut empty = [];
        assert!(Pin::new(&mut wrapper)
            .poll_read(&mut cx, &mut ReadBuf::new(&mut empty))
            .is_ready());
        drop(sender);
    }
}

#[cfg(test)]
mod socket_tests {
    use super::*;
    use futures::StreamExt;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        time::{timeout, Duration},
    };
    async fn sockets() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (client, server) = tokio::join!(TcpStream::connect(address), listener.accept());
        (client.unwrap(), server.unwrap().0)
    }
    fn client_records(secret: &[u8; 32], plaintext: &[u8]) -> Vec<u8> {
        let cipher =
            ChaCha20Poly1305::new(Array::from_slice(&compute_read_key(secret).unwrap()));
        let mut wire = Vec::new();
        for (counter, chunk) in plaintext.chunks(1024).enumerate() {
            let mut aad = [0; 2];
            LittleEndian::write_u16(&mut aad, chunk.len() as u16);
            let mut nonce = [0; 12];
            LittleEndian::write_u64(&mut nonce[4..], counter as u64);
            let mut value = chunk.to_vec();
            let tag = cipher
                .encrypt_in_place_detached(Nonce::from_slice(&nonce), &aad, &mut value)
                .unwrap();
            wire.extend(aad);
            wire.extend(value);
            wire.extend(tag);
        }
        wire
    }
    #[tokio::test]
    async fn session_arriving_before_final_verify_response_does_not_encrypt_that_response() {
        timeout(Duration::from_secs(5), async {
            let (mut client, server) = sockets().await;
            let (mut transport, mut incoming, outgoing, session, _, _) =
                EncryptedStream::new(server);
            let id = Uuid::new_v4();
            let secret = [23; 32];
            let controller = transport.controller_id.clone();
            session
                .send(Session {
                    controller_id: id,
                    shared_secret: secret,
                })
                .unwrap();
            // Force the scheduling order that previously encrypted M4 too early.
            let waker = futures::task::noop_waker();
            let mut cx = Context::from_waker(&waker);
            let mut bytes = [0; 64];
            let mut buffer = ReadBuf::new(&mut bytes);
            assert!(Pin::new(&mut transport)
                .poll_read(&mut cx, &mut buffer)
                .is_pending());
            assert!(transport.shared_secret.is_some());
            assert!(controller.read().unwrap().is_none());
            let task = tokio::spawn(transport);
            let response = b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\n\x06\x01\x04";
            for chunk in response.chunks(7) {
                outgoing.try_send(chunk.to_vec()).unwrap();
            }
            let mut actual = vec![0; response.len()];
            client.read_exact(&mut actual).await.unwrap();
            assert_eq!(actual, response);
            assert!(controller.read().unwrap().is_none());
            let request = b"GET /accessories HTTP/1.1\r\n\r\n";
            client
                .write_all(&client_records(&secret, request))
                .await
                .unwrap();
            assert_eq!(incoming.next().await.unwrap(), request);
            assert_eq!(*controller.read().unwrap(), Some(id));
            outgoing.try_send(b"encrypted response".to_vec()).unwrap();
            let mut aad = [0; 2];
            client.read_exact(&mut aad).await.unwrap();
            let len = LittleEndian::read_u16(&aad) as usize;
            let mut body = vec![0; len];
            client.read_exact(&mut body).await.unwrap();
            let mut tag = [0; 16];
            client.read_exact(&mut tag).await.unwrap();
            ChaCha20Poly1305::new(Array::from_slice(
                &compute_write_key(&secret).unwrap(),
            ))
            .decrypt_in_place_detached(
                Nonce::from_slice(&[0; 12]),
                &aad,
                &mut body,
                Tag::from_slice(&tag),
            )
            .unwrap();
            assert_eq!(body, b"encrypted response");
            client.shutdown().await.unwrap();
            task.await.unwrap().unwrap();
        })
        .await
        .expect("verification boundary test timed out");
    }
    #[tokio::test]
    async fn corrupt_first_encrypted_request_never_publishes_controller_identity() {
        timeout(Duration::from_secs(5), async {
            let (mut client, server) = sockets().await;
            let (transport, mut incoming, _outgoing, session, _, _) = EncryptedStream::new(server);
            let controller = transport.controller_id.clone();
            let secret = [24; 32];
            session
                .send(Session {
                    controller_id: Uuid::new_v4(),
                    shared_secret: secret,
                })
                .unwrap();
            let task = tokio::spawn(transport);
            let mut wire = client_records(&secret, b"GET /accessories");
            *wire.last_mut().unwrap() ^= 1;
            client.write_all(&wire).await.unwrap();
            assert_eq!(
                task.await.unwrap().unwrap_err().kind(),
                ErrorKind::InvalidData
            );
            assert!(controller.read().unwrap().is_none());
            assert!(incoming.next().await.is_none());
        })
        .await
        .expect("bad record was not rejected");
    }
    #[tokio::test]
    async fn localhost_bidirectional_encrypted_records_and_peer_eof() {
        timeout(Duration::from_secs(5), async {
            let (mut client, server) = sockets().await;
            let (mut transport, mut incoming, outgoing, _session, _, _) =
                EncryptedStream::new(server);
            // Tests the established encrypted transport only, not Pair Verify.
            let secret = [17; 32];
            transport.shared_secret = Some(secret);
            let task = tokio::spawn(transport);
            let plaintext: Vec<u8> = (0..2500).map(|v| (v % 251) as u8).collect();
            let wire = client_records(&secret, &plaintext);
            for chunk in wire.chunks(17) {
                client.write_all(chunk).await.unwrap();
            }
            let mut received = Vec::new();
            while received.len() < plaintext.len() {
                received.extend(incoming.next().await.unwrap());
            }
            assert_eq!(received, plaintext);
            outgoing.try_send(plaintext.clone()).unwrap();
            let cipher = ChaCha20Poly1305::new(Array::from_slice(
                &compute_write_key(&secret).unwrap(),
            ));
            let mut received = Vec::new();
            for counter in 0..3u64 {
                let mut aad = [0; 2];
                client.read_exact(&mut aad).await.unwrap();
                let len = LittleEndian::read_u16(&aad) as usize;
                assert!(len > 0 && len <= 1024);
                let mut body = vec![0; len];
                client.read_exact(&mut body).await.unwrap();
                let mut tag = [0; 16];
                client.read_exact(&mut tag).await.unwrap();
                let mut nonce = [0; 12];
                LittleEndian::write_u64(&mut nonce[4..], counter);
                cipher
                    .decrypt_in_place_detached(
                        Nonce::from_slice(&nonce),
                        &aad,
                        &mut body,
                        Tag::from_slice(&tag),
                    )
                    .unwrap();
                received.extend(body);
            }
            assert_eq!(received, plaintext);
            client.shutdown().await.unwrap();
            task.await.unwrap().unwrap();
            assert!(incoming.next().await.is_none());
        })
        .await
        .expect("loopback encrypted exchange timed out");
    }
    #[tokio::test]
    async fn output_overflow_closes_real_socket_and_releases_queued_bytes() {
        timeout(Duration::from_secs(5), async {
            let (mut client, server) = sockets().await;
            let (transport, _incoming, outgoing, _session, _, _) = EncryptedStream::new(server);
            // Queue before polling transport to deterministically simulate a
            // consumer that is not draining; no OS socket-buffer timing guess.
            for _ in 0..4 {
                outgoing.try_send(vec![0; OUTPUT_MESSAGE_BYTES]).unwrap();
            }
            assert!(outgoing.try_send(vec![1]).is_err());
            assert_eq!(
                transport.await.unwrap_err().kind(),
                ErrorKind::ConnectionAborted
            );
            let mut byte = [0];
            assert_eq!(client.read(&mut byte).await.unwrap(), 0);
            assert_eq!(outgoing.used.load(std::sync::atomic::Ordering::Acquire), 0);
        })
        .await
        .expect("overflow did not close loopback socket");
    }
}
