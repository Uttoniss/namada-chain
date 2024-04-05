//! Token validity predicate queries

use namada_core::address::Address;
use namada_core::token;
use namada_state::{DBIter, StorageHasher, DB};
use namada_token::{read_denom, read_total_supply};

use crate::queries::RequestCtx;

router! {TOKEN,
    ( "denomination" / [addr: Address] ) -> Option<token::Denomination> = denomination,
    ( "total_supply" / [addr: Address] ) -> token::Amount = total_supply,
}

/// Get the number of decimal places (in base 10) for a
/// token specified by `addr`.
fn denomination<D, H, V, T>(
    ctx: RequestCtx<'_, D, H, V, T>,
    addr: Address,
) -> namada_storage::Result<Option<token::Denomination>>
where
    D: 'static + DB + for<'iter> DBIter<'iter> + Sync,
    H: 'static + StorageHasher + Sync,
{
    read_denom(ctx.state, &addr)
}

/// Get the total supply for a token address
fn total_supply<D, H, V, T>(
    ctx: RequestCtx<'_, D, H, V, T>,
    addr: Address,
) -> namada_storage::Result<token::Amount>
where
    D: 'static + DB + for<'iter> DBIter<'iter> + Sync,
    H: 'static + StorageHasher + Sync,
{
    read_total_supply(ctx.state, &addr)
}

#[cfg(any(test, feature = "async-client"))]
pub mod client_only_methods {
    use borsh::BorshDeserialize;
    use namada_core::address::Address;
    use namada_core::token;
    use namada_token::storage_key::balance_key;

    use super::Token;
    use crate::queries::{Client, RPC};

    impl Token {
        /// Get the balance of the given `token` belonging to the given `owner`.
        pub async fn balance<CLIENT>(
            &self,
            client: &CLIENT,
            token: &Address,
            owner: &Address,
        ) -> Result<token::Amount, <CLIENT as Client>::Error>
        where
            CLIENT: Client + Sync,
        {
            let balance_key = balance_key(token, owner);
            let response = RPC
                .shell()
                .storage_value(client, None, None, false, &balance_key)
                .await?;

            let balance = if response.data.is_empty() {
                tracing::error!("Balance error empty: {:?}", response);
                token::Amount::zero()
            } else {
                match token::Amount::try_from_slice(&response.data) {
                    Ok(balance) => {
                        tracing::info!("Balance ok: {:?}", balance);
                        balance
                    }
                    Err(e) => {
                        tracing::error!("Balance error inner: {:?}", response);
                        tracing::error!("Error inner: {}", e.to_string());
                        token::Amount::zero()
                    },
                }
                    
            };
            Ok(balance)
        }
    }
}
