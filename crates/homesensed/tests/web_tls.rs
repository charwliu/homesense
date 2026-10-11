#[test]
fn tls_requires_explicit_local_address_and_exact_https_origin() {
    use homesensed::web_tls::validate_origin;
    for (origin, address) in [
        ("https://localhost:8443", "127.0.0.1:8443"),
        ("https://homesense.example", "192.168.1.10:443"),
        ("https://homesense.example:8443", "[fd00::1]:8443"),
    ] {
        assert!(validate_origin(origin, address.parse().unwrap()).is_ok());
    }
    for (origin, address) in [
        ("https://localhost:443", "127.0.0.1:443"),
        ("https://LOCALHOST:8443", "127.0.0.1:8443"),
        ("https://localhost:08443", "127.0.0.1:8443"),
        ("https://localhost:8443", "0.0.0.0:8443"),
        ("https://localhost:8443", "8.8.8.8:8443"),
        ("https://localhost:8443", "[::]:8443"),
        ("http://localhost:8443", "127.0.0.1:8443"),
        ("https://user@localhost:8443", "127.0.0.1:8443"),
        ("https://localhost:8443/", "127.0.0.1:8443"),
        ("https://localhost:8443/path", "127.0.0.1:8443"),
        ("https://localhost:8443?token=x", "127.0.0.1:8443"),
        ("https://localhost:8444", "127.0.0.1:8443"),
    ] {
        assert!(
            validate_origin(origin, address.parse().unwrap()).is_err(),
            "{origin} {address}"
        );
    }
}
