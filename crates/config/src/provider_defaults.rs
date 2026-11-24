//! Named model aliases and curated rosters.
//! Provider default seeds are generated from the shared descriptor owner;
//! model aliases and completion rosters are generated from the same reviewed catalog.

#[cfg(test)]
pub(crate) use crate::catalog::reviewed::constants::*;
pub(crate) use crate::descriptors::defaults::*;

pub(crate) const MOONSHOT_CN_BASE_URL: &str = "https://api.moonshot.cn/v1";

pub(crate) const XIAOMI_MIMO_PAY_AS_YOU_GO_BASE_URL: &str = "https://api.xiaomimimo.com/v1";

pub(crate) const XIAOMI_MIMO_TOKEN_PLAN_CN_BASE_URL: &str =
    "https://token-plan-cn.xiaomimimo.com/v1";
pub(crate) const XIAOMI_MIMO_TOKEN_PLAN_SGP_BASE_URL: &str = DEFAULT_XIAOMI_MIMO_BASE_URL;
pub(crate) const XIAOMI_MIMO_TOKEN_PLAN_AMS_BASE_URL: &str =
    "https://token-plan-ams.xiaomimimo.com/v1";
