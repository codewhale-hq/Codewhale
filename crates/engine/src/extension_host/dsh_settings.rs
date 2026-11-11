use super::dsh_broker::{BrokerCall, BrokerFuture, not_available};
use super::protocol::DshService;

pub(super) fn serve(_call: BrokerCall) -> BrokerFuture {
    not_available(DshService::Settings)
}
