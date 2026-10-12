//! Offline old/new AEAD API witness using synthetic keys only.
//! Compile with --extern chacha20poly1305=RLIB and -L dependency=TARGET/debug/deps.
//! The intentionally deprecated comparison side is outside workspace targets.
use chacha20poly1305::aead::{AeadInOut, AeadInPlace, KeyInit, array::Array};
use chacha20poly1305::ChaCha20Poly1305;
fn main() {
    let mut cases=0;
    for len in [0usize,1,15,16,17,1023,1024,1025] {
        for counter in [0u64,1,255,u64::MAX-1] {
            for authenticated_length in [false,true] {
                let key=std::array::from_fn::<u8,32,_>(|i|i as u8 ^ counter as u8);
                let old=ChaCha20Poly1305::new(Array::from_slice(&key));
                let new=ChaCha20Poly1305::new((&key).into());
                let mut nonce=vec![0u8;4];nonce.extend(counter.to_le_bytes());
                let aad=if authenticated_length {(len as u16).to_le_bytes().to_vec()}else {vec![]};
                let plain=(0..len).map(|i|(i.wrapping_mul(17)^len) as u8).collect::<Vec<_>>();
                let mut old_body=plain.clone();let mut new_body=plain.clone();
                let old_tag=AeadInPlace::encrypt_in_place_detached(&old,Array::from_slice(&nonce),&aad,&mut old_body).unwrap();
                let new_tag=AeadInOut::encrypt_inout_detached(&new,nonce.as_slice().try_into().unwrap(),&aad,new_body.as_mut_slice().into()).unwrap();
                assert_eq!(old_body,new_body);assert_eq!(old_tag,new_tag);
                let mut old_plain=new_body.clone();let mut new_plain=old_body.clone();
                AeadInPlace::decrypt_in_place_detached(&old,Array::from_slice(&nonce),&aad,&mut old_plain,&new_tag).unwrap();
                AeadInOut::decrypt_inout_detached(&new,nonce.as_slice().try_into().unwrap(),&aad,new_plain.as_mut_slice().into(),&old_tag).unwrap();
                assert_eq!(old_plain,plain);assert_eq!(new_plain,plain);
                let mut bad_tag=new_tag;bad_tag[0]^=1;
                let mut old_bad=old_body.clone();let mut new_bad=new_body.clone();
                assert!(AeadInPlace::decrypt_in_place_detached(&old,Array::from_slice(&nonce),&aad,&mut old_bad,&bad_tag).is_err());
                assert!(AeadInOut::decrypt_inout_detached(&new,nonce.as_slice().try_into().unwrap(),&aad,new_bad.as_mut_slice().into(),&bad_tag).is_err());
                let mut bad_aad=aad.clone();bad_aad.push(1);
                assert!(AeadInPlace::decrypt_in_place_detached(&old,Array::from_slice(&nonce),&bad_aad,&mut old_body,&old_tag).is_err());
                assert!(AeadInOut::decrypt_inout_detached(&new,nonce.as_slice().try_into().unwrap(),&bad_aad,new_body.as_mut_slice().into(),&new_tag).is_err());
                cases+=1;
            }
        }
    }
    println!("{cases} cases: ciphertext/tag equality, cross-decrypt, wrong-tag and wrong-AAD rejection passed");
}
