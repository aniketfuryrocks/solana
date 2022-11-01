use {
    actix_web::{http::StatusCode, web, App, HttpResponse, HttpServer, Responder},
    serde::{Deserialize, Serialize},
    serde_json::json,
    solana_client::{
        connection_cache::ConnectionCache, rpc_config::RpcSendTransactionConfig,
        thin_client::ThinClient, tpu_connection::TpuConnection,
    },
    solana_sdk::{
        client::AsyncClient, signature::Signature, transaction::Transaction,
        transport::TransportError,
    },
    std::{
        net::{SocketAddr, ToSocketAddrs},
        sync::Arc,
    },
};

pub struct SendTransactionConfig {}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RpcMethod {
    SendTransaction,
    RequestAirdrop,
}

/// According to <https://www.jsonrpc.org/specification#overview>
#[derive(Debug, Deserialize, Serialize)]
pub struct JsonRpcReq {
    pub method: RpcMethod,
    pub params: serde_json::Value,
}

#[derive(Debug, Deserialize, Serialize)]
pub enum JsonRpcRes {
    Err(serde_json::Value),
    Ok(serde_json::Value),
}

impl Responder for JsonRpcRes {
    type Body = String;

    fn respond_to(self, _: &actix_web::HttpRequest) -> HttpResponse<Self::Body> {
        let mut res = json!({
            "jsonrpc" : "2.0",
            // TODO: add id
        });

        match self {
            Self::Err(error) => {
                res["error"] = error;
                HttpResponse::new(StatusCode::from_u16(500).unwrap()).set_body(res.to_string())
            }
            Self::Ok(result) => {
                res["result"] = result;
                HttpResponse::new(StatusCode::OK).set_body(res.to_string())
            }
        }
    }
}

impl<T: serde::Serialize> TryFrom<Result<T, TransportError>> for JsonRpcRes {
    type Error = serde_json::Error;

    fn try_from(result: Result<T, TransportError>) -> Result<Self, Self::Error> {
        Ok(match result {
            Ok(value) => Self::Ok(serde_json::to_value(value)?),
            Err(error) => Self::Err(serde_json::to_value(error.unwrap())?),
        })
    }
}

/// A bridge between clients and tpu
pub struct LightBridge {
    tpu_addr: SocketAddr,
    thin_client: ThinClient,
    connection_cache: Arc<ConnectionCache>,
}

impl LightBridge {
    pub fn new(rpc_addr: SocketAddr, tpu_addr: SocketAddr, connection_pool_size: usize) -> Self {
        let connection_cache = Arc::new(ConnectionCache::new(connection_pool_size));
        let thin_client = ThinClient::new(rpc_addr, tpu_addr.clone(), connection_cache.clone());

        Self {
            thin_client,
            connection_cache,
            tpu_addr,
        }
    }

    pub fn forward_transaction(
        &self,
        transaction: Transaction,
    ) -> Result<Signature, TransportError> {
        self.thin_client.async_send_transaction(transaction)
    }

    fn send_transaction(
        &self,
        params: (String, Option<RpcSendTransactionConfig>),
    ) -> Result<Signature, TransportError> {
        let base_58_decoded = bs58::decode(&params.0).into_vec().unwrap();
        let conn = self.connection_cache.get_connection(&self.tpu_addr);
        conn.send_wire_transaction_async(base_58_decoded).unwrap();
        Ok(Signature::new_unique())
    }

    pub async fn execute_rpc_request(
        &self,
        JsonRpcReq { method, params }: JsonRpcReq,
    ) -> JsonRpcRes {
        match method {
            RpcMethod::SendTransaction => self
                .send_transaction(serde_json::from_value(params).unwrap())
                .try_into()
                .unwrap(),
            RpcMethod::RequestAirdrop => todo!(),
        }
    }

    /// List for `JsonRpc` requests
    pub async fn start_server(self, addr: impl ToSocketAddrs) -> Result<(), std::io::Error> {
        let bridge = Arc::new(self);

        HttpServer::new(move || {
            App::new()
                .app_data(web::Data::new(bridge.clone()))
                .route("/", web::post().to(Self::rpc_route))
        })
        .bind(addr)?
        .run()
        .await
    }

    async fn rpc_route(
        json_rpc_req: web::Json<JsonRpcReq>,
        state: web::Data<Arc<LightBridge>>,
    ) -> JsonRpcRes {
        state.execute_rpc_request(json_rpc_req.0).await
    }
}

#[cfg(test)]
mod tests {

    use {
        crate::bridge::LightBridge,
        borsh::{BorshDeserialize, BorshSerialize},
        solana_sdk::{
            instruction::Instruction, message::Message, pubkey::Pubkey, signature::Signer,
            signer::keypair::Keypair, transaction::Transaction,
        },
    };

    const RPC_ADDR: &str = "127.0.0.1:8899";
    const TPU_ADDR: &str = "127.0.0.1:1027";
    const CONNECTION_POOL_SIZE: usize = 1;

    #[derive(BorshSerialize, BorshDeserialize)]
    enum BankInstruction {
        Initialize,
        Deposit { lamports: u64 },
        Withdraw { lamports: u64 },
    }

    #[test]
    fn initialize_light_bridge() {
        let _light_bridge = LightBridge::new(
            RPC_ADDR.parse().unwrap(),
            TPU_ADDR.parse().unwrap(),
            CONNECTION_POOL_SIZE,
        );
    }

    #[test]
    fn test_forward_transaction() {
        let light_bridge = LightBridge::new(
            RPC_ADDR.parse().unwrap(),
            TPU_ADDR.parse().unwrap(),
            CONNECTION_POOL_SIZE,
        );

        let program_id = Pubkey::new_unique();
        let payer = Keypair::new();
        let bankins = BankInstruction::Initialize;
        let instruction = Instruction::new_with_borsh(program_id, &bankins, vec![]);

        let message = Message::new(&[instruction], Some(&payer.pubkey()));
        let blockhash = light_bridge
            .thin_client
            .rpc_client()
            .get_latest_blockhash()
            .unwrap();

        let tx = Transaction::new(&[&payer], message, blockhash);
        let x = light_bridge.forward_transaction(tx).unwrap();

        println!("{}", x);
    }
}
