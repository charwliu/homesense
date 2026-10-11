use sha_old::{Digest, Sha512};
fn main() {
    for n in 1..=32u8 {
        let salt = [n; 16]; let a = [n;64]; let b = [n+1;64];
        let username = b"Pair-Setup"; let password = b"123-45-678";
        let old_group = &srp_old::groups::G_3072;
        let new_group = &srp_new::groups::G_3072;
        let x = srp_old::client::srp_private_key::<Sha512>(username,password,&salt);
        let old_client = srp_old::client::SrpClient::<Sha512>::new(&a,old_group);
        let v = old_client.get_password_verifier(&x);
        let new_client = srp_new::client::SrpClient::<sha_new::Sha512>::new(new_group);
        assert_eq!(v,new_client.compute_verifier(username,password,&salt));
        let ap = old_client.get_a_pub();
        assert_eq!(ap,new_client.compute_public_ephemeral(&a));
        let user = srp_old::server::UserRecord {username,salt:&salt,verifier:&v};
        let old_server = srp_old::server::SrpServer::<Sha512>::new(&user,&ap,&b,old_group).unwrap();
        let new_server = srp_new::server::SrpServer::<sha_new::Sha512>::new(new_group);
        let bp = old_server.get_b_pub();
        assert_eq!(bp,new_server.compute_public_ephemeral(&b,&v));
        let key = old_server.get_key();
        assert_eq!(key.to_vec(),Sha512::digest(new_server.process_reply(&b,&v,&ap).unwrap().key()).to_vec());
        assert_eq!(key.to_vec(),old_client.process_reply(&x,&bp).unwrap().get_key().to_vec());
        assert_eq!(key.to_vec(),Sha512::digest(new_client.process_reply(&a,username,password,&salt,&bp).unwrap().key()).to_vec());
        assert!(new_server.process_reply(&b,&v,&[0]).is_err());
        assert!(new_client.process_reply(&a,username,password,&salt,&[0]).is_err());
    }
    println!("32 deterministic cross-version verifier/public-key/session-key vectors passed; zero public keys rejected");
}
