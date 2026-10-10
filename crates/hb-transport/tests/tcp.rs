use hb_transport::{Connection, PreparedFrame, Receive};
use homesense_core::{
    BusAddress, Channel, Command, ExecutionContext, ModuleSpec, ProtocolProfile, Verification,
};
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
    time::Duration,
};

#[test]
fn localhost_socket_sends_one_frame_and_receives_split_feedback() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = [0; 5];
        stream.read_exact(&mut request).unwrap();
        assert_eq!(request, [0xAE, 3, 1, 0xF1, 0xEE]);
        stream.write_all(&[0xAE, 3]).unwrap();
        stream.write_all(&[0xD1, 0xF1, 0xEE]).unwrap();
    });
    let stream = std::net::TcpStream::connect_timeout(&address, Duration::from_secs(2)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut connection = Connection::for_simulation(stream, 9);
    let module = ModuleSpec::new(
        BusAddress::new(3).unwrap(),
        ProtocolProfile::GtC8Rl,
        Verification::Simulated,
    )
    .unwrap();
    let frame = PreparedFrame::prepare(
        &module,
        Channel::new(1).unwrap(),
        Command::Switch(true),
        ExecutionContext::Simulation,
        9,
    )
    .unwrap();
    connection.send(&frame, 100, || 0).unwrap();
    let mut frames = vec![];
    for _ in 0..4 {
        match connection.receive().unwrap() {
            Receive::Frames {
                transport_epoch,
                frames: batch,
            } => {
                assert_eq!(transport_epoch, 9);
                frames.extend(batch);
            }
            Receive::Disconnected { .. } => break,
            Receive::Idle => panic!("local server did not respond"),
        }
    }
    server.join().unwrap();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].bytes(), [0xAE, 3, 0xD1, 0xF1, 0xEE]);
}
