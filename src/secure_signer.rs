#[macro_use]
extern crate anyhow;

mod keys;
mod datafeed;
mod attest;
mod routes;
mod worker_api;
mod leader_api;
mod common_api;
mod beacon_types;
mod beacon_signing;

use warp::Filter;


#[tokio::main]
async fn main() {
    let port = std::env::args().nth(1).unwrap_or("3031".into()).parse::<u16>().expect("BAD PORT");
    println!("Starting SGX-Signer enclave HTTP server: localhost:{}", port);
    let routes = 

        // --------- Compatible with Web3Signer ---------
        // https://consensys.github.io/web3signer/web3signer-eth2.html

        // Endpoint to securely import a BLS sk 
        // curl -X POST localhost:3031/eth/v1/keystores -H "Content-Type: application/json"  -d '{"ct_bls_sk_hex": "0x123123", "bls_pk_hex": "0x123", "encrypting_pk_hex": "0x123"}'  
        routes::bls_key_import_route()

        // Endpoint to list pks of saved bls keys that were imported into the enclave
        // curl -X GET localhost:3031/eth/v1/keystores
        .or(routes::list_imported_bls_keys_route())

        // Endpoint to request a signature using BLS sk 
        // curl -X POST localhost:3031/eth/v1/sign/bls -H "Content-Type: application/json"  -d '{"msg_hex": "0xdeadbeef", "bls_pk_hex": "0x123"}'  
        .or(routes::bls_sign_route())

        // --------- Addition to Web3Signer ---------

        // Endpoint to perform remote attestation with intel using a supplied PK
        // curl -X POST localhost:3031/eth/v1/remote-attestation -H "Content-Type: application/json"  -d '{"pub_key": "123123"}'
        .or(routes::epid_remote_attestation_route())

        // Endpoint to securely generate and save an ETH sk 
        // curl -X POST localhost:3031/eth/v1/keygen/eth
        .or(routes::eth_key_gen_route())

        // Endpoint to list the pks of all the generated ETH keys
        // curl -X GET localhost:3031/eth/v1/keygen/eth
        .or(routes::list_generated_eth_keys_route())

        // Endpoint to securely generate and save a BLS sk 
        // curl -X POST localhost:3031/eth/v1/keygen/bls
        .or(routes::bls_key_gen_route())

        // Endpoint to list pks of saved bls keys that were generated in the enclave
        // curl -X GET localhost:3031/eth/v1/keygen/bls
        .or(routes::list_generated_bls_keys_route());


    warp::serve(routes).run(([127, 0, 0, 1], port)).await;
}



#[cfg(test)]
mod signing_api_tests {
    use super::*;
    use crate::keys::{new_bls_key, new_eth_key, CIPHER_SUITE, aggregate_uniform_bls_sigs};
    use crate::attest::{AttestationEvidence, fetch_dummy_evidence};
    use crate::routes::*;
    use crate::common_api::*;
    use ecies::{decrypt, encrypt};
    use blst::min_pk::{SecretKey, PublicKey, Signature};
    use ecies::PublicKey as EthPublicKey;
    use ecies::SecretKey as EthSecretKey;
    use std::fs;
    use serde_json;
    use crate::beacon_signing::slash_resistance_tests::*;
    use crate::beacon_signing::non_slashing_signing_tests::*;

    async fn mock_secure_sign_bls_route(bls_pk: &String, json_req: &String) -> warp::http::Response<bytes::Bytes> {
        let filter = bls_sign_route();
        let uri = format!("/api/v1/eth2/sign/{}", bls_pk);

        let res = warp::test::request()
            .method("POST")
            .path(&uri)
            .body(&json_req)
            .reply(&filter)
            .await;
        res
    }

    #[tokio::test]
    async fn test_bls_sign_route_block_type() {
        // clear state
        fs::remove_dir_all("./etc");

        // new keypair
        let bls_pk_hex = setup_keypair();

        // mock data for a BLOCK request
        let json_req = mock_propose_block_request("0xfe");
        let resp = mock_secure_sign_bls_route(&bls_pk_hex, &json_req).await;
        assert_eq!(resp.status(), 200);

        // mock data for a BLOCK request (attempt a slashable offense - non-increasing slot)
        let json_req = mock_propose_block_request("0xfe");
        let resp = mock_secure_sign_bls_route(&bls_pk_hex, &json_req).await;
        assert_eq!(resp.status(), 412);

        // mock data for a BLOCK request (attempt a slashable offense - decreasing slot)
        let json_req = mock_propose_block_request("0xfd");
        let resp = mock_secure_sign_bls_route(&bls_pk_hex, &json_req).await;
        assert_eq!(resp.status(), 412);

        // mock data for a BLOCK request 
        let json_req = mock_propose_block_request("0xff");
        let resp = mock_secure_sign_bls_route(&bls_pk_hex, &json_req).await;
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_bls_sign_route_attestation_type() {
        // clear state
        fs::remove_dir_all("./etc");

        // new keypair
        let bls_pk_hex = setup_keypair();

        // mock data for ATTESTATION request
        let json_req = mock_attestation_request("0x0a", "0x0b");
        let resp = mock_secure_sign_bls_route(&bls_pk_hex, &json_req).await;
        assert_eq!(resp.status(), 200);

        // mock data for ATTESTATION request (attempt a slashable offense - decreasing source)
        let json_req = mock_attestation_request("0x00", "0x0c");
        let resp = mock_secure_sign_bls_route(&bls_pk_hex, &json_req).await;
        assert_eq!(resp.status(), 412);

        // mock data for ATTESTATION request (attempt a slashable offense - non-increasing target)
        let json_req = mock_attestation_request("0x0a", "0x0b");
        let resp = mock_secure_sign_bls_route(&bls_pk_hex, &json_req).await;
        assert_eq!(resp.status(), 412);

        // mock data for ATTESTATION request (non-increasing source + increasing target)
        let json_req = mock_attestation_request("0x0a", "0x0c");
        let resp = mock_secure_sign_bls_route(&bls_pk_hex, &json_req).await;
        assert_eq!(resp.status(), 200);

        // mock data for ATTESTATION request (increasing source + increasing target)
        let json_req = mock_attestation_request("0x0b", "0x0d");
        let resp = mock_secure_sign_bls_route(&bls_pk_hex, &json_req).await;
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_bls_sign_route_randao_reveal_type() {
        // clear state
        fs::remove_dir_all("./etc");

        // new keypair
        let bls_pk_hex = setup_keypair();

        // mock data for RANDAO_REVEAL request
        let json_req = mock_randao_reveal_request("0x0a");
        let resp = mock_secure_sign_bls_route(&bls_pk_hex, &json_req).await;
        println!("{:?}", resp);
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_bls_sign_route_aggregate_and_proof_type() {
        // clear state
        fs::remove_dir_all("./etc");

        // new keypair
        let bls_pk_hex = setup_keypair();

        // mock data for RANDAO_REVEAL request
        let json_req = mock_aggregate_and_proof_request("0x0", "0x1");
        let resp = mock_secure_sign_bls_route(&bls_pk_hex, &json_req).await;
        println!("{:?}", resp);
        assert_eq!(resp.status(), 200);
    }

}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{new_bls_key, new_eth_key, CIPHER_SUITE, aggregate_uniform_bls_sigs};
    use crate::attest::{AttestationEvidence, fetch_dummy_evidence};
    use crate::routes::*;
    use crate::common_api::*;
    use ecies::{decrypt, encrypt};
    use blst::min_pk::{SecretKey, PublicKey, Signature};
    use ecies::PublicKey as EthPublicKey;
    use ecies::SecretKey as EthSecretKey;
    use std::fs;
    use serde_json;


    async fn call_eth_key_gen_route() -> KeyGenResponse {
        let filter = eth_key_gen_route();

        // mock the request
        let res = warp::test::request()
            .method("POST")
            .path("/eth/v1/keygen/secp256k1")
            .reply(&filter)
            .await;

        // println!("resp: {:?}", res.body());
        assert_eq!(res.status(), 200);

        // parse the resp
        let resp: KeyGenResponse = serde_json::from_slice(&res.body()).unwrap();
        resp
    }

    #[tokio::test]
    async fn test_call_eth_key_gen_route() {
        fs::remove_dir_all("./etc");
        let resp = call_eth_key_gen_route().await;
        println!("resp: {:?}", resp);
    }

    async fn mock_request_bls_key_import_route() -> KeyImportResponse {
        // 1) generate ETH secret key in enclave
        let resp = call_eth_key_gen_route().await;
        let enclave_eth_pk_hex = &resp.data[0].message;
        let enclave_eth_pk_bytes = hex::decode(&enclave_eth_pk_hex).unwrap();

        // 2) request enclave to do remote attestation
        // let resp = enclave_remote_attestation();

        // 3) verify evidence
        // todo

        // 4) extract ETH pub key
        // todo

        // 5) locally generate BLS key to import
        let bls_sk = new_bls_key().unwrap();

        // 6) encrypt BLS key with ETH pub key
        let ct_bls_sk = encrypt(&enclave_eth_pk_bytes, &bls_sk.serialize()).unwrap();
        let ct_bls_sk_hex = hex::encode(ct_bls_sk);
        let bls_pk_hex = hex::encode(bls_sk.sk_to_pk().serialize());

        // 7) make payload to send /eth/v1/keystores POST request
        let req = KeyImportRequest {
            ct_bls_sk_hex: ct_bls_sk_hex,
            bls_pk_hex: bls_pk_hex.clone(),
            encrypting_pk_hex: enclave_eth_pk_hex.clone(),
        };
        println!("making bls key import req: {:?}", req);

        // 8) make the actual request
        let filter = bls_key_import_route();
        let res = warp::test::request()
            .method("POST")
            .header("accept", "application/json")
            .path("/eth/v1/keystores")
            .json(&req)
            .reply(&filter)
            .await;


        println!{"{:?}", res.body()};
        assert_eq!(res.status(), 200);

        let resp: KeyImportResponse = serde_json::from_slice(&res.body()).unwrap();

        assert_eq!(resp.data[0].status, "imported".to_string());
        assert_eq!(resp.data[0].message, bls_pk_hex);
        resp
    }


    #[tokio::test]
    async fn test_request_bls_key_import_route() {
        let resp = mock_request_bls_key_import_route().await;
    }

    async fn mock_request_bls_key_list_route() -> ListKeysResponse {
        let filter = list_imported_bls_keys_route();
        let res = warp::test::request()
            .method("GET")
            .path("/eth/v1/keystores")
            .reply(&filter)
            .await;

        assert_eq!(res.status(), 200);
        let resp: ListKeysResponse = serde_json::from_slice(&res.body()).unwrap();
        resp
    }

    #[tokio::test]
    async fn test_list_imported_bls_keys_route() {
        // clear any existing local keys
        fs::remove_dir_all("./etc");
        let key_gen_resp = mock_request_bls_key_import_route().await;
        let bls_pk_hex = key_gen_resp.data[0].message.clone();
        assert_eq!(key_gen_resp.data.len(), 1);

        let list_keys_resp = mock_request_bls_key_list_route().await;
        assert_eq!(list_keys_resp.data.len(), 1);
        assert_eq!(list_keys_resp.data[0].pubkey, bls_pk_hex);
    }


    async fn mock_request_bls_key_gen_route() -> KeyGenResponse {
        let filter = bls_key_gen_route();
        let res = warp::test::request()
            .method("POST")
            .path("/eth/v1/keygen/bls")
            .reply(&filter)
            .await;

        println!{"{:?}", res.body()};
        assert_eq!(res.status(), 200);

        let resp: KeyGenResponse = serde_json::from_slice(&res.body()).unwrap();

        assert_eq!(resp.data[0].status, "generated".to_string());
        resp
    }

    async fn mock_request_generated_bls_key_list_route() -> ListKeysResponse {
        let filter = list_generated_bls_keys_route();
        let res = warp::test::request()
            .method("GET")
            .path("/eth/v1/keygen/bls")
            .reply(&filter)
            .await;

        assert_eq!(res.status(), 200);
        let resp: ListKeysResponse = serde_json::from_slice(&res.body()).unwrap();
        resp
    }

    #[tokio::test]
    async fn test_bls_key_gen_route() {
        // clear any existing local keys
        fs::remove_dir_all("./etc");
        let key_gen_resp = mock_request_bls_key_gen_route().await;
        let bls_pk_hex = key_gen_resp.data[0].message.clone();
        assert_eq!(key_gen_resp.data.len(), 1);

        let list_keys_resp = mock_request_generated_bls_key_list_route().await;
        assert_eq!(list_keys_resp.data.len(), 1);
        assert_eq!(list_keys_resp.data[0].pubkey, bls_pk_hex);
    }
}
