//! Production native carrier profile -> actual SecuritySlot -> protected TCP
//! REGISTER -> retained MT request/reply -> exact socket/SA drain and reuse.
//! The synthetic IP peers are userspace smoltcp owners, without a host socket.
use super::exercise_selected_security_suite;
macro_rules! suite {
    ($name:ident,$alg:literal,$enc:literal) => {
        #[tokio::test(flavor = "current_thread")]
        async fn $name() {
            exercise_selected_security_suite($alg, $enc).await;
        }
    };
}
suite!(native_register_aes_md5, "hmac-md5-96", "aes-cbc");
suite!(native_register_3des_sha1, "hmac-sha-1-96", "des-ede3-cbc");
suite!(native_register_3des_md5, "hmac-md5-96", "des-ede3-cbc");
suite!(native_register_null_sha1, "hmac-sha-1-96", "null");
suite!(native_register_null_md5, "hmac-md5-96", "null");
