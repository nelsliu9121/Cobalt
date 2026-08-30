use crate::model::{PurchaseType, Quote};
use kobo_json::{ObjectBuilder, Value};
use kobo_sdk::StoreError;

pub const MARKER_KEY: &str = "commerce.unresolved.v1";
pub const MAX_MARKER_BYTES: usize = 2_048;

const MARKER_VERSION: usize = 1;
const MAX_ALIAS_BYTES: usize = 128;
const MARKER_FIELDS: [&str; 10] = [
    "version",
    "account_scope",
    "title_id",
    "title_alias",
    "episode_id",
    "episode_alias",
    "purchase_type",
    "quoted_price",
    "pre_mutation_spendable_coin",
    "pre_mutation_title_gifts",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AccountScope([u8; 16]);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScopeError {
    InvalidLength,
    InvalidHex,
}

impl AccountScope {
    pub fn from_bytes(encoded: &[u8]) -> Result<Self, ScopeError> {
        if encoded.len() != 32 {
            return Err(ScopeError::InvalidLength);
        }
        let mut decoded = [0_u8; 16];
        for (slot, pair) in decoded.iter_mut().zip(encoded.chunks_exact(2)) {
            *slot = decode_nibble(pair[0])
                .and_then(|high| decode_nibble(pair[1]).map(|low| high << 4 | low))
                .ok_or(ScopeError::InvalidHex)?;
        }
        Ok(Self(decoded))
    }

    #[must_use]
    pub fn to_hex(self) -> [u8; 32] {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut encoded = [0_u8; 32];
        for (byte, pair) in self.0.iter().zip(encoded.chunks_exact_mut(2)) {
            pair[0] = HEX[usize::from(byte >> 4)];
            pair[1] = HEX[usize::from(byte & 0x0f)];
        }
        encoded
    }
}

fn decode_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnresolvedMutationV1 {
    pub account_scope: AccountScope,
    pub title_id: usize,
    pub title_alias: String,
    pub episode_id: usize,
    pub episode_alias: String,
    pub purchase_type: PurchaseType,
    pub quoted_price: usize,
    pub pre_mutation_spendable_coin: Option<usize>,
    pub pre_mutation_title_gifts: Option<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkerError {
    TooLarge,
    InvalidUtf8,
    InvalidJson,
    InvalidShape,
    InvalidVersion,
    InvalidField(&'static str),
}

pub fn encode_marker(marker: &UnresolvedMutationV1) -> Result<Vec<u8>, MarkerError> {
    validate_alias(&marker.title_alias, "title_alias")?;
    validate_alias(&marker.episode_alias, "episode_alias")?;
    validate_snapshots(marker)?;

    let scope = marker.account_scope.to_hex();
    let scope = std::str::from_utf8(&scope).expect("lowercase hexadecimal is UTF-8");
    let value = ObjectBuilder::new()
        .set("version", json_usize(MARKER_VERSION))
        .set("account_scope", scope)
        .set("title_id", json_usize(marker.title_id))
        .set("title_alias", marker.title_alias.as_str())
        .set("episode_id", json_usize(marker.episode_id))
        .set("episode_alias", marker.episode_alias.as_str())
        .set("purchase_type", marker.purchase_type.as_remote())
        .set("quoted_price", json_usize(marker.quoted_price))
        .set(
            "pre_mutation_spendable_coin",
            optional_usize(marker.pre_mutation_spendable_coin),
        )
        .set(
            "pre_mutation_title_gifts",
            optional_usize(marker.pre_mutation_title_gifts),
        )
        .build()
        .to_json()
        .into_bytes();
    if value.len() > MAX_MARKER_BYTES {
        return Err(MarkerError::TooLarge);
    }
    Ok(value)
}

pub fn decode_marker(encoded: &[u8]) -> Result<UnresolvedMutationV1, MarkerError> {
    if encoded.len() > MAX_MARKER_BYTES {
        return Err(MarkerError::TooLarge);
    }
    let text = std::str::from_utf8(encoded).map_err(|_| MarkerError::InvalidUtf8)?;
    let root = kobo_json::parse(text).map_err(|_| MarkerError::InvalidJson)?;
    let fields = strict_marker_fields(&root)?;
    if parse_usize(field(fields, "version")?, "version")? != MARKER_VERSION {
        return Err(MarkerError::InvalidVersion);
    }
    let scope = field(fields, "account_scope")?
        .as_str()
        .ok_or(MarkerError::InvalidField("account_scope"))
        .and_then(|value| {
            AccountScope::from_bytes(value.as_bytes())
                .map_err(|_| MarkerError::InvalidField("account_scope"))
        })?;
    let title_alias = parse_alias(field(fields, "title_alias")?, "title_alias")?;
    let episode_alias = parse_alias(field(fields, "episode_alias")?, "episode_alias")?;
    let purchase_type = match field(fields, "purchase_type")?.as_str() {
        Some("RENT_GIFT") => PurchaseType::RentGift,
        Some("RENT") => PurchaseType::Rent,
        Some("POSSESSION") => PurchaseType::Possession,
        _ => return Err(MarkerError::InvalidField("purchase_type")),
    };
    let marker = UnresolvedMutationV1 {
        account_scope: scope,
        title_id: parse_usize(field(fields, "title_id")?, "title_id")?,
        title_alias,
        episode_id: parse_usize(field(fields, "episode_id")?, "episode_id")?,
        episode_alias,
        purchase_type,
        quoted_price: parse_usize(field(fields, "quoted_price")?, "quoted_price")?,
        pre_mutation_spendable_coin: parse_optional_usize(
            field(fields, "pre_mutation_spendable_coin")?,
            "pre_mutation_spendable_coin",
        )?,
        pre_mutation_title_gifts: parse_optional_usize(
            field(fields, "pre_mutation_title_gifts")?,
            "pre_mutation_title_gifts",
        )?,
    };
    validate_snapshots(&marker)?;
    Ok(marker)
}

fn strict_marker_fields(value: &Value) -> Result<&[(String, Value)], MarkerError> {
    let Value::Object(fields) = value else {
        return Err(MarkerError::InvalidShape);
    };
    if fields.len() != MARKER_FIELDS.len()
        || fields.iter().any(|(name, _)| {
            !MARKER_FIELDS.contains(&name.as_str())
                || fields.iter().filter(|(other, _)| other == name).count() != 1
        })
    {
        return Err(MarkerError::InvalidShape);
    }
    Ok(fields)
}

fn field<'a>(fields: &'a [(String, Value)], name: &'static str) -> Result<&'a Value, MarkerError> {
    fields
        .iter()
        .find(|(candidate, _)| candidate == name)
        .map(|(_, value)| value)
        .ok_or(MarkerError::InvalidField(name))
}

fn parse_usize(value: &Value, name: &'static str) -> Result<usize, MarkerError> {
    value
        .as_integer_str()
        .and_then(|value| value.parse().ok())
        .ok_or(MarkerError::InvalidField(name))
}

fn parse_optional_usize(value: &Value, name: &'static str) -> Result<Option<usize>, MarkerError> {
    if matches!(value, Value::Null) {
        Ok(None)
    } else {
        parse_usize(value, name).map(Some)
    }
}

fn parse_alias(value: &Value, name: &'static str) -> Result<String, MarkerError> {
    let value = value.as_str().ok_or(MarkerError::InvalidField(name))?;
    validate_alias(value, name)?;
    Ok(value.to_owned())
}

fn validate_alias(value: &str, name: &'static str) -> Result<(), MarkerError> {
    if value.is_empty()
        || value.len() > MAX_ALIAS_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(MarkerError::InvalidField(name));
    }
    Ok(())
}

fn validate_snapshots(marker: &UnresolvedMutationV1) -> Result<(), MarkerError> {
    let valid = match marker.purchase_type {
        PurchaseType::RentGift => {
            marker.pre_mutation_spendable_coin.is_none()
                && marker.pre_mutation_title_gifts.is_some()
        }
        PurchaseType::Rent | PurchaseType::Possession => {
            marker.pre_mutation_spendable_coin.is_some()
                && marker.pre_mutation_title_gifts.is_none()
        }
    };
    if valid {
        Ok(())
    } else {
        Err(MarkerError::InvalidShape)
    }
}

fn json_usize(value: usize) -> Value {
    kobo_json::parse(&value.to_string()).expect("a usize is always a JSON integer")
}

fn optional_usize(value: Option<usize>) -> Value {
    value.map_or(Value::Null, json_usize)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Selection {
    pub title_id: usize,
    pub title_alias: String,
    pub episode_id: usize,
    pub episode_alias: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Authentication {
    Unknown,
    Authenticated(AccountScope),
    SignedOut,
    Expired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Connectivity {
    Unknown,
    Online,
    Offline,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    UseGift,
    Rent,
    Buy,
    Cancel,
}

impl Action {
    const fn purchase_type(self) -> Option<PurchaseType> {
        match self {
            Self::UseGift => Some(PurchaseType::RentGift),
            Self::Rent => Some(PurchaseType::Rent),
            Self::Buy => Some(PurchaseType::Possession),
            Self::Cancel => None,
        }
    }

    const fn quote_type(self) -> Option<PurchaseType> {
        match self {
            Self::UseGift | Self::Rent => Some(PurchaseType::Rent),
            Self::Buy => Some(PurchaseType::Possession),
            Self::Cancel => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommerceCommand {
    SaveMarker(Vec<u8>),
    FetchQuote {
        selection: Selection,
        purchase: PurchaseType,
    },
    Post(UnresolvedMutationV1),
    RefreshContent(Selection),
    ForgetMarker,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CommerceEffects {
    pub command: Option<CommerceCommand>,
    pub refresh_wallet: bool,
    pub refresh_gifts: bool,
    pub redraw: bool,
}

impl CommerceEffects {
    fn command(command: CommerceCommand) -> Self {
        Self {
            command: Some(command),
            ..Self::default()
        }
    }

    fn redraw() -> Self {
        Self {
            redraw: true,
            ..Self::default()
        }
    }

    fn reconcile(selection: Selection, refresh_wallet: bool, refresh_gifts: bool) -> Self {
        Self {
            command: Some(CommerceCommand::RefreshContent(selection)),
            refresh_wallet,
            refresh_gifts,
            redraw: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommerceState {
    LoadingSafetyState,
    Idle,
    Quoting,
    Choosing,
    Requoting,
    PersistingIntent,
    Mutating,
    Reconciling,
    ClearingIntent,
    AcceptedButStale,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PostOutcome {
    ExplicitRejection,
    Accepted,
    Ambiguous,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reconciliation {
    Conclusive,
    Incomplete,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuoteControl {
    pub action: Action,
    pub label: String,
    pub enabled: bool,
    pub disabled_reason: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuotePresentation {
    controls: [QuoteControl; 4],
    pub quote_changed: bool,
}

impl QuotePresentation {
    #[must_use]
    pub fn control(&self, action: Action) -> &QuoteControl {
        self.controls
            .iter()
            .find(|control| control.action == action)
            .expect("every commerce action has one presentation control")
    }
}

fn quote_presentation(
    quote: &Quote,
    spendable_coin: Option<usize>,
    title_gifts: Option<usize>,
    active_rental: bool,
    quote_changed: bool,
) -> QuotePresentation {
    QuotePresentation {
        controls: [
            gift_control(quote, title_gifts, active_rental),
            paid_control(Action::Rent, quote, spendable_coin, active_rental),
            paid_control(Action::Buy, quote, spendable_coin, active_rental),
            QuoteControl {
                action: Action::Cancel,
                label: "Cancel".to_owned(),
                enabled: true,
                disabled_reason: None,
            },
        ],
        quote_changed,
    }
}

fn gift_control(quote: &Quote, title_gifts: Option<usize>, active_rental: bool) -> QuoteControl {
    let disabled_reason = if active_rental {
        Some("Active rental".to_owned())
    } else if quote.is_available {
        Some("Access already available".to_owned())
    } else if !quote.is_rent_gift {
        Some("Gift not available for this episode".to_owned())
    } else {
        match title_gifts {
            None => Some("Gift balance unavailable".to_owned()),
            Some(0) => Some("No Gifts for this title".to_owned()),
            Some(_) => None,
        }
    };
    QuoteControl {
        action: Action::UseGift,
        label: "Use Gift".to_owned(),
        enabled: disabled_reason.is_none(),
        disabled_reason,
    }
}

fn paid_control(
    action: Action,
    quote: &Quote,
    spendable_coin: Option<usize>,
    active_rental: bool,
) -> QuoteControl {
    let price = match action {
        Action::Rent => quote.rent_coin,
        Action::Buy => quote.possession_coin,
        Action::UseGift | Action::Cancel => {
            unreachable!("paid controls are only Rent and Buy")
        }
    };
    let label = match action {
        Action::Rent => format!("Rent · {price} coins"),
        Action::Buy => format!("Buy · {price} coins"),
        Action::UseGift | Action::Cancel => unreachable!(),
    };
    let valid_price = match action {
        Action::Rent => quote.rent_price().is_some(),
        Action::Buy => quote.purchase_price().is_some(),
        Action::UseGift | Action::Cancel => unreachable!(),
    };
    let disabled_reason = if active_rental {
        Some("Active rental".to_owned())
    } else if quote.is_available {
        Some("Access already available".to_owned())
    } else if quote.coin_kind != "COIN" {
        Some("Coin payment unavailable".to_owned())
    } else if !valid_price {
        Some("Purchase price unavailable".to_owned())
    } else {
        match spendable_coin {
            None => Some("Coin balance unavailable".to_owned()),
            Some(balance) if balance < price => Some(format!("Need {price} coins")),
            Some(_) => None,
        }
    };
    QuoteControl {
        action,
        label,
        enabled: disabled_reason.is_none(),
        disabled_reason,
    }
}

#[derive(Clone, Debug)]
struct Choosing {
    account_scope: AccountScope,
    selection: Selection,
    quote: Quote,
    spendable_coin: Option<usize>,
    title_gifts: Option<usize>,
    presentation: QuotePresentation,
}

#[derive(Clone, Debug)]
enum Flow {
    LoadingSafetyState,
    Idle,
    Quoting {
        account_scope: AccountScope,
        selection: Selection,
    },
    Choosing(Choosing),
    Requoting {
        previous: Choosing,
        action: Action,
    },
    PersistingIntent {
        marker: UnresolvedMutationV1,
        previous: Choosing,
    },
    Mutating(UnresolvedMutationV1),
    Reconciling {
        account_scope: AccountScope,
        marker: Option<UnresolvedMutationV1>,
        selection: Selection,
    },
    ClearingIntent(UnresolvedMutationV1),
    AcceptedButStale,
}

#[derive(Clone, Debug)]
enum MarkerLoad {
    Pending,
    Empty,
    Valid(UnresolvedMutationV1),
    Invalid,
}

pub struct Commerce {
    flow: Flow,
    authentication: Authentication,
    connectivity: Connectivity,
    marker: MarkerLoad,
}

impl Default for Commerce {
    fn default() -> Self {
        Self::new()
    }
}

impl Commerce {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            flow: Flow::LoadingSafetyState,
            authentication: Authentication::Unknown,
            connectivity: Connectivity::Unknown,
            marker: MarkerLoad::Pending,
        }
    }

    #[must_use]
    pub const fn state(&self) -> CommerceState {
        match self.flow {
            Flow::LoadingSafetyState => CommerceState::LoadingSafetyState,
            Flow::Idle => CommerceState::Idle,
            Flow::Quoting { .. } => CommerceState::Quoting,
            Flow::Choosing(_) => CommerceState::Choosing,
            Flow::Requoting { .. } => CommerceState::Requoting,
            Flow::PersistingIntent { .. } => CommerceState::PersistingIntent,
            Flow::Mutating(_) => CommerceState::Mutating,
            Flow::Reconciling { .. } => CommerceState::Reconciling,
            Flow::ClearingIntent(_) => CommerceState::ClearingIntent,
            Flow::AcceptedButStale => CommerceState::AcceptedButStale,
        }
    }

    #[must_use]
    pub fn marker_belongs_to_another_account(&self) -> bool {
        matches!(
            (&self.flow, self.authentication, &self.marker),
            (
                Flow::AcceptedButStale,
                Authentication::Authenticated(current_scope),
                MarkerLoad::Valid(marker)
            ) if marker.account_scope != current_scope
        )
    }

    #[must_use]
    pub fn quote_presentation(&self) -> Option<&QuotePresentation> {
        match &self.flow {
            Flow::Choosing(choosing) => Some(&choosing.presentation),
            Flow::Requoting { previous, .. } | Flow::PersistingIntent { previous, .. } => {
                Some(&previous.presentation)
            }
            Flow::LoadingSafetyState
            | Flow::Idle
            | Flow::Quoting { .. }
            | Flow::Mutating(_)
            | Flow::Reconciling { .. }
            | Flow::ClearingIntent(_)
            | Flow::AcceptedButStale => None,
        }
    }
    pub fn cancel_unpersisted(&mut self) -> CommerceEffects {
        let flow = std::mem::replace(&mut self.flow, Flow::LoadingSafetyState);
        match flow {
            Flow::Quoting { .. } | Flow::Choosing(_) | Flow::Requoting { .. } => {
                self.flow = Flow::Idle;
                CommerceEffects::redraw()
            }
            other => {
                self.flow = other;
                CommerceEffects::default()
            }
        }
    }

    #[must_use]
    pub fn reconciliation_marker(&self) -> Option<&UnresolvedMutationV1> {
        match &self.flow {
            Flow::Reconciling {
                marker: Some(marker),
                ..
            } => Some(marker),
            Flow::LoadingSafetyState
            | Flow::Idle
            | Flow::Quoting { .. }
            | Flow::Choosing(_)
            | Flow::Requoting { .. }
            | Flow::PersistingIntent { .. }
            | Flow::Mutating(_)
            | Flow::Reconciling { marker: None, .. }
            | Flow::ClearingIntent(_)
            | Flow::AcceptedButStale => None,
        }
    }

    pub fn safety_changed(
        &mut self,
        authentication: Authentication,
        connectivity: Connectivity,
    ) -> CommerceEffects {
        self.authentication = authentication;
        self.connectivity = connectivity;

        if matches!(
            self.flow,
            Flow::PersistingIntent { .. }
                | Flow::Mutating(_)
                | Flow::Reconciling {
                    marker: Some(_),
                    ..
                }
                | Flow::ClearingIntent(_)
        ) && !self.marker_scope_is_current()
        {
            if let Flow::PersistingIntent { marker, .. } = &self.flow {
                // The save request may have reached durable storage even when its
                // acknowledgement is lost. Treat that intent as unresolved.
                self.marker = MarkerLoad::Valid(marker.clone());
            }
            self.flow = Flow::AcceptedButStale;
            return CommerceEffects::redraw();
        }

        if self
            .volatile_scope()
            .is_some_and(|scope| self.current_scope() != Some(scope))
        {
            self.flow = Flow::LoadingSafetyState;
            return self.settle_loaded_marker();
        }

        if matches!(self.flow, Flow::LoadingSafetyState | Flow::AcceptedButStale) {
            return self.settle_loaded_marker();
        }
        CommerceEffects::default()
    }

    pub fn marker_loaded(&mut self, value: Option<&[u8]>) -> CommerceEffects {
        if !matches!(self.marker, MarkerLoad::Pending)
            || !matches!(self.flow, Flow::LoadingSafetyState)
        {
            return CommerceEffects::default();
        }
        self.marker = match value {
            None => MarkerLoad::Empty,
            Some(value) => decode_marker(value).map_or(MarkerLoad::Invalid, MarkerLoad::Valid),
        };
        self.settle_loaded_marker()
    }

    pub fn begin_quote(&mut self, selection: Selection, purchase: PurchaseType) -> CommerceEffects {
        if !matches!(self.flow, Flow::Idle)
            || validate_alias(&selection.title_alias, "title_alias").is_err()
            || validate_alias(&selection.episode_alias, "episode_alias").is_err()
        {
            return CommerceEffects::default();
        }
        let Some(account_scope) = self.current_scope() else {
            return CommerceEffects::default();
        };
        let purchase = quote_type(purchase);
        self.flow = Flow::Quoting {
            account_scope,
            selection: selection.clone(),
        };
        CommerceEffects::command(CommerceCommand::FetchQuote {
            selection,
            purchase,
        })
    }

    pub fn quote_received(
        &mut self,
        quote: Quote,
        spendable_coin: Option<usize>,
        title_gifts: Option<usize>,
        active_rental: bool,
    ) -> CommerceEffects {
        let flow = std::mem::replace(&mut self.flow, Flow::LoadingSafetyState);
        match flow {
            Flow::Quoting {
                account_scope,
                selection,
            } => {
                if self.current_scope() != Some(account_scope) || !quote_matches(&selection, &quote)
                {
                    self.flow = Flow::LoadingSafetyState;
                    return self.settle_loaded_marker();
                }
                if quote.is_available {
                    self.flow = Flow::Reconciling {
                        account_scope,
                        marker: None,
                        selection: selection.clone(),
                    };
                    return CommerceEffects::reconcile(selection, false, false);
                }
                let presentation =
                    quote_presentation(&quote, spendable_coin, title_gifts, active_rental, false);
                self.flow = Flow::Choosing(Choosing {
                    account_scope,
                    selection,
                    quote,
                    spendable_coin,
                    title_gifts,
                    presentation,
                });
                CommerceEffects::redraw()
            }
            Flow::Requoting { previous, action } => {
                if self.current_scope() != Some(previous.account_scope) {
                    self.flow = Flow::LoadingSafetyState;
                    return self.settle_loaded_marker();
                }
                if !quote_matches(&previous.selection, &quote) {
                    self.flow = Flow::Idle;
                    return CommerceEffects::redraw();
                }
                if quote.is_available {
                    self.flow = Flow::Reconciling {
                        account_scope: previous.account_scope,
                        marker: None,
                        selection: previous.selection.clone(),
                    };
                    return CommerceEffects::reconcile(previous.selection, false, false);
                }
                let changed = quote != previous.quote;
                let presentation =
                    quote_presentation(&quote, spendable_coin, title_gifts, active_rental, changed);
                let current = Choosing {
                    account_scope: previous.account_scope,
                    selection: previous.selection,
                    quote,
                    spendable_coin,
                    title_gifts,
                    presentation,
                };
                if changed || !current.presentation.control(action).enabled {
                    self.flow = Flow::Choosing(current);
                    CommerceEffects::redraw()
                } else {
                    self.persist(current, action)
                }
            }
            other => {
                self.flow = other;
                CommerceEffects::default()
            }
        }
    }

    pub fn quote_failed(&mut self) -> CommerceEffects {
        if matches!(self.flow, Flow::Quoting { .. } | Flow::Requoting { .. }) {
            self.flow = Flow::Idle;
            CommerceEffects::redraw()
        } else {
            CommerceEffects::default()
        }
    }

    pub fn choose(&mut self, action: Action) -> CommerceEffects {
        let flow = std::mem::replace(&mut self.flow, Flow::LoadingSafetyState);
        let Flow::Choosing(choosing) = flow else {
            self.flow = flow;
            return CommerceEffects::default();
        };
        if action == Action::Cancel {
            self.flow = Flow::Idle;
            return CommerceEffects::redraw();
        }
        if self.current_scope() != Some(choosing.account_scope)
            || !choosing.presentation.control(action).enabled
        {
            self.flow = Flow::Choosing(choosing);
            return CommerceEffects::default();
        }
        let requested = action
            .quote_type()
            .expect("Cancel returned before quote selection");
        let selection = choosing.selection.clone();
        self.flow = Flow::Requoting {
            previous: choosing,
            action,
        };
        CommerceEffects::command(CommerceCommand::FetchQuote {
            selection,
            purchase: requested,
        })
    }

    pub fn marker_saved(&mut self, key: &str) -> CommerceEffects {
        if key != MARKER_KEY {
            return CommerceEffects::default();
        }
        let flow = std::mem::replace(&mut self.flow, Flow::LoadingSafetyState);
        let Flow::PersistingIntent { marker, previous } = flow else {
            self.flow = flow;
            return CommerceEffects::default();
        };
        self.marker = MarkerLoad::Valid(marker.clone());
        if self.current_scope() != Some(marker.account_scope) {
            self.flow = Flow::AcceptedButStale;
            return CommerceEffects::redraw();
        }
        drop(previous);
        self.flow = Flow::Mutating(marker.clone());
        CommerceEffects::command(CommerceCommand::Post(marker))
    }

    pub fn marker_forgotten(&mut self, key: &str) -> CommerceEffects {
        if key != MARKER_KEY {
            return CommerceEffects::default();
        }
        let flow = std::mem::replace(&mut self.flow, Flow::LoadingSafetyState);
        let Flow::ClearingIntent(marker) = flow else {
            self.flow = flow;
            return CommerceEffects::default();
        };
        if self.current_scope() != Some(marker.account_scope) {
            self.flow = Flow::AcceptedButStale;
            return CommerceEffects::default();
        }
        self.marker = MarkerLoad::Empty;
        self.flow = Flow::Idle;
        CommerceEffects::redraw()
    }

    pub fn store_denied(&mut self, _error: StoreError) -> CommerceEffects {
        let flow = std::mem::replace(&mut self.flow, Flow::LoadingSafetyState);
        match flow {
            Flow::PersistingIntent { previous, .. } => {
                self.flow = Flow::Choosing(previous);
            }
            Flow::ClearingIntent(marker) => {
                self.marker = MarkerLoad::Valid(marker);
                self.flow = Flow::AcceptedButStale;
            }
            Flow::LoadingSafetyState if matches!(self.marker, MarkerLoad::Pending) => {
                self.marker = MarkerLoad::Invalid;
                self.flow = Flow::AcceptedButStale;
            }
            other => {
                self.flow = other;
                return CommerceEffects::default();
            }
        }
        CommerceEffects::redraw()
    }

    pub fn mutation_finished(&mut self, outcome: PostOutcome) -> CommerceEffects {
        let flow = std::mem::replace(&mut self.flow, Flow::LoadingSafetyState);
        let Flow::Mutating(marker) = flow else {
            self.flow = flow;
            return CommerceEffects::default();
        };
        if self.current_scope() != Some(marker.account_scope) {
            self.flow = Flow::AcceptedButStale;
            return CommerceEffects::redraw();
        }
        if outcome == PostOutcome::ExplicitRejection {
            self.flow = Flow::ClearingIntent(marker);
            return CommerceEffects::command(CommerceCommand::ForgetMarker);
        }
        let selection = marker_selection(&marker);
        let (refresh_wallet, refresh_gifts) = match outcome {
            PostOutcome::Accepted => match marker.purchase_type {
                PurchaseType::RentGift => (false, true),
                PurchaseType::Rent | PurchaseType::Possession => (true, false),
            },
            PostOutcome::Ambiguous => (true, true),
            PostOutcome::ExplicitRejection => unreachable!(),
        };
        let account_scope = marker.account_scope;
        self.flow = Flow::Reconciling {
            account_scope,
            marker: Some(marker),
            selection: selection.clone(),
        };
        CommerceEffects::reconcile(selection, refresh_wallet, refresh_gifts)
    }

    pub fn reconciled(
        &mut self,
        account_scope: AccountScope,
        result: Reconciliation,
    ) -> CommerceEffects {
        let flow = std::mem::replace(&mut self.flow, Flow::LoadingSafetyState);
        let Flow::Reconciling {
            account_scope: expected_scope,
            marker,
            selection,
        } = flow
        else {
            self.flow = flow;
            return CommerceEffects::default();
        };
        if account_scope != expected_scope || self.current_scope() != Some(expected_scope) {
            self.flow = if marker.is_some() {
                Flow::AcceptedButStale
            } else {
                Flow::Reconciling {
                    account_scope: expected_scope,
                    marker,
                    selection,
                }
            };
            return CommerceEffects::default();
        }
        let Some(marker) = marker else {
            self.flow = Flow::Idle;
            return CommerceEffects::redraw();
        };
        if account_scope != marker.account_scope
            || self.current_scope() != Some(marker.account_scope)
            || result == Reconciliation::Incomplete
        {
            self.flow = Flow::AcceptedButStale;
            return CommerceEffects::redraw();
        }
        drop(selection);
        self.flow = Flow::ClearingIntent(marker);
        CommerceEffects::command(CommerceCommand::ForgetMarker)
    }

    pub fn refresh_status(&mut self) -> CommerceEffects {
        if !matches!(self.flow, Flow::AcceptedButStale) {
            return CommerceEffects::default();
        }
        let MarkerLoad::Valid(marker) = &self.marker else {
            return CommerceEffects::default();
        };
        if self.current_scope() != Some(marker.account_scope) {
            return CommerceEffects::default();
        }
        let marker = marker.clone();
        let selection = marker_selection(&marker);
        self.flow = Flow::Reconciling {
            account_scope: marker.account_scope,
            marker: Some(marker),
            selection: selection.clone(),
        };
        CommerceEffects::reconcile(selection, true, true)
    }

    fn settle_loaded_marker(&mut self) -> CommerceEffects {
        match &self.marker {
            MarkerLoad::Pending => {
                self.flow = Flow::LoadingSafetyState;
                CommerceEffects::default()
            }
            MarkerLoad::Empty => {
                if self.current_scope().is_some() {
                    self.flow = Flow::Idle;
                    CommerceEffects::redraw()
                } else {
                    self.flow = Flow::LoadingSafetyState;
                    CommerceEffects::default()
                }
            }
            MarkerLoad::Valid(marker) => {
                let marker = marker.clone();
                match self.current_scope() {
                    Some(scope) if scope == marker.account_scope => {
                        let selection = marker_selection(&marker);
                        self.flow = Flow::Reconciling {
                            account_scope: marker.account_scope,
                            marker: Some(marker),
                            selection: selection.clone(),
                        };
                        CommerceEffects::reconcile(selection, true, true)
                    }
                    None if matches!(
                        (self.authentication, self.connectivity),
                        (Authentication::Unknown, _) | (_, Connectivity::Unknown)
                    ) =>
                    {
                        self.flow = Flow::LoadingSafetyState;
                        CommerceEffects::default()
                    }
                    Some(_) | None => {
                        self.flow = Flow::AcceptedButStale;
                        CommerceEffects::redraw()
                    }
                }
            }
            MarkerLoad::Invalid => {
                if matches!(
                    (self.authentication, self.connectivity),
                    (Authentication::Unknown, _) | (_, Connectivity::Unknown)
                ) {
                    self.flow = Flow::LoadingSafetyState;
                    CommerceEffects::default()
                } else {
                    self.flow = Flow::AcceptedButStale;
                    CommerceEffects::redraw()
                }
            }
        }
    }

    fn persist(&mut self, choosing: Choosing, action: Action) -> CommerceEffects {
        debug_assert_eq!(self.current_scope(), Some(choosing.account_scope));
        let account_scope = choosing.account_scope;
        let purchase_type = action
            .purchase_type()
            .expect("Cancel returned before marker construction");
        let quoted_price = match action {
            Action::UseGift => 0,
            Action::Rent => choosing.quote.rent_coin,
            Action::Buy => choosing.quote.possession_coin,
            Action::Cancel => unreachable!(),
        };
        let marker = UnresolvedMutationV1 {
            account_scope,
            title_id: choosing.selection.title_id,
            title_alias: choosing.selection.title_alias.clone(),
            episode_id: choosing.selection.episode_id,
            episode_alias: choosing.selection.episode_alias.clone(),
            purchase_type,
            quoted_price,
            pre_mutation_spendable_coin: match action {
                Action::Rent | Action::Buy => choosing.spendable_coin,
                Action::UseGift | Action::Cancel => None,
            },
            pre_mutation_title_gifts: match action {
                Action::UseGift => choosing.title_gifts,
                Action::Rent | Action::Buy | Action::Cancel => None,
            },
        };
        let Ok(encoded) = encode_marker(&marker) else {
            self.flow = Flow::Choosing(choosing);
            return CommerceEffects::default();
        };
        self.flow = Flow::PersistingIntent {
            marker,
            previous: choosing,
        };
        CommerceEffects::command(CommerceCommand::SaveMarker(encoded))
    }

    fn volatile_scope(&self) -> Option<AccountScope> {
        match &self.flow {
            Flow::Quoting { account_scope, .. }
            | Flow::Reconciling {
                account_scope,
                marker: None,
                ..
            } => Some(*account_scope),
            Flow::Choosing(choosing) => Some(choosing.account_scope),
            Flow::Requoting { previous, .. } => Some(previous.account_scope),
            Flow::LoadingSafetyState
            | Flow::Idle
            | Flow::PersistingIntent { .. }
            | Flow::Mutating(_)
            | Flow::Reconciling {
                marker: Some(_), ..
            }
            | Flow::ClearingIntent(_)
            | Flow::AcceptedButStale => None,
        }
    }

    fn current_scope(&self) -> Option<AccountScope> {
        match (self.authentication, self.connectivity) {
            (Authentication::Authenticated(scope), Connectivity::Online) => Some(scope),
            _ => None,
        }
    }

    fn marker_scope_is_current(&self) -> bool {
        let scope = match &self.flow {
            Flow::PersistingIntent { marker, .. }
            | Flow::Mutating(marker)
            | Flow::ClearingIntent(marker)
            | Flow::Reconciling {
                marker: Some(marker),
                ..
            } => Some(marker.account_scope),
            _ => match &self.marker {
                MarkerLoad::Valid(marker) => Some(marker.account_scope),
                MarkerLoad::Pending | MarkerLoad::Empty | MarkerLoad::Invalid => None,
            },
        };
        scope.is_some_and(|scope| self.current_scope() == Some(scope))
    }
}

fn quote_type(purchase: PurchaseType) -> PurchaseType {
    match purchase {
        PurchaseType::RentGift | PurchaseType::Rent => PurchaseType::Rent,
        PurchaseType::Possession => PurchaseType::Possession,
    }
}

fn quote_matches(selection: &Selection, quote: &Quote) -> bool {
    selection.title_id == quote.content_id
        && selection.episode_id == quote.episode_id
        && selection.title_alias == quote.content_alias
        && selection.episode_alias == quote.episode_alias
}

fn marker_selection(marker: &UnresolvedMutationV1) -> Selection {
    Selection {
        title_id: marker.title_id,
        title_alias: marker.title_alias.clone(),
        episode_id: marker.episode_id,
        episode_alias: marker.episode_alias.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{PurchaseType, Quote};
    use kobo_sdk::StoreError;

    fn scope() -> AccountScope {
        AccountScope::from_bytes(b"00112233445566778899aabbccddeeff").expect("scope")
    }

    fn marker(purchase_type: PurchaseType) -> UnresolvedMutationV1 {
        UnresolvedMutationV1 {
            account_scope: scope(),
            title_id: 7,
            title_alias: "comic-a".to_owned(),
            episode_id: 11,
            episode_alias: "episode-1".to_owned(),
            purchase_type,
            quoted_price: usize::from(purchase_type != PurchaseType::RentGift) * 5,
            pre_mutation_spendable_coin: (purchase_type != PurchaseType::RentGift).then_some(23),
            pre_mutation_title_gifts: (purchase_type == PurchaseType::RentGift).then_some(2),
        }
    }

    fn marker_json(purchase_type: PurchaseType) -> String {
        String::from_utf8(encode_marker(&marker(purchase_type)).expect("encode marker"))
            .expect("marker JSON is UTF-8")
    }

    fn selection() -> Selection {
        Selection {
            title_id: 7,
            title_alias: "comic-a".to_owned(),
            episode_id: 11,
            episode_alias: "episode-1".to_owned(),
        }
    }

    fn quote() -> Quote {
        Quote {
            content_id: 7,
            episode_id: 11,
            content_alias: "comic-a".to_owned(),
            episode_alias: "episode-1".to_owned(),
            is_available: false,
            coin_kind: "COIN".to_owned(),
            rent_coin: 5,
            possession_coin: 9,
            permanent_coin: Some(9),
            is_rent_gift: true,
            is_possession_gift: false,
        }
    }

    fn ready() -> Commerce {
        let mut commerce = Commerce::new();
        commerce.safety_changed(Authentication::Authenticated(scope()), Connectivity::Online);
        commerce.marker_loaded(None);
        assert_eq!(commerce.state(), CommerceState::Idle);
        commerce
    }

    fn choosing(purchase: PurchaseType) -> Commerce {
        let mut commerce = ready();
        assert!(matches!(
            commerce.begin_quote(selection(), purchase).command,
            Some(CommerceCommand::FetchQuote { .. })
        ));
        let effects = commerce.quote_received(quote(), Some(23), Some(2), false);
        assert!(effects.command.is_none());
        assert_eq!(commerce.state(), CommerceState::Choosing);
        commerce
    }

    fn persisting(action: Action, purchase: PurchaseType) -> Commerce {
        let mut commerce = choosing(purchase);
        assert!(matches!(
            commerce.choose(action).command,
            Some(CommerceCommand::FetchQuote { .. })
        ));
        assert!(matches!(
            commerce
                .quote_received(quote(), Some(23), Some(2), false)
                .command,
            Some(CommerceCommand::SaveMarker(_))
        ));
        assert_eq!(commerce.state(), CommerceState::PersistingIntent);
        commerce
    }

    fn mutating(action: Action, purchase: PurchaseType) -> Commerce {
        let mut commerce = persisting(action, purchase);
        assert!(matches!(
            commerce.marker_saved(MARKER_KEY).command,
            Some(CommerceCommand::Post(_))
        ));
        assert_eq!(commerce.state(), CommerceState::Mutating);
        commerce
    }

    #[test]
    fn account_scope_requires_exact_lowercase_hex() {
        let valid = b"00112233445566778899aabbccddeeff";
        assert_eq!(
            AccountScope::from_bytes(valid)
                .expect("valid")
                .to_hex()
                .as_slice(),
            valid
        );

        for invalid in [
            b"00112233445566778899aabbccddeeFf".as_slice(),
            b"00112233445566778899aabbccddee".as_slice(),
            b"00112233445566778899aabbccddeeff0".as_slice(),
            b"00112233445566778899aabbccddeefg".as_slice(),
        ] {
            assert!(
                AccountScope::from_bytes(invalid).is_err(),
                "accepted {invalid:?}"
            );
        }
    }

    #[test]
    fn marker_round_trips_each_supported_snapshot_shape() {
        for purchase_type in [
            PurchaseType::RentGift,
            PurchaseType::Rent,
            PurchaseType::Possession,
        ] {
            let expected = marker(purchase_type);
            let encoded = encode_marker(&expected).expect("encode marker");
            assert_eq!(decode_marker(&encoded).expect("decode marker"), expected);
        }
    }

    #[test]
    fn marker_rejects_unknown_missing_duplicate_and_mistyped_fields() {
        let valid = marker_json(PurchaseType::Rent);
        let cases = [
            valid.replace("\"version\":1", "\"version\":2"),
            valid.replace("\"title_id\":7,", ""),
            valid.replace("\"title_id\":7", "\"title_id\":7,\"title_id\":7"),
            valid.replace("\"episode_id\":11", "\"episode_id\":\"11\""),
            valid.replace("\"quoted_price\":5", "\"quoted_price\":5.0"),
            valid.replace("\"version\":1", "\"version\":1,\"future\":true"),
        ];

        for invalid in cases {
            assert!(
                decode_marker(invalid.as_bytes()).is_err(),
                "accepted {invalid}"
            );
        }
    }

    #[test]
    fn marker_rejects_invalid_aliases_on_encode_and_decode() {
        let mut value = marker(PurchaseType::Rent);
        value.title_alias = "a".repeat(128);
        value.episode_alias = "b".repeat(128);
        assert!(encode_marker(&value).is_ok());

        for alias in [String::new(), "a".repeat(129), "not/an/alias".to_owned()] {
            value.title_alias.clone_from(&alias);
            assert!(encode_marker(&value).is_err(), "encoded alias {alias:?}");
        }

        let oversized = marker_json(PurchaseType::Rent).replace("comic-a", &"a".repeat(129));
        assert!(decode_marker(oversized.as_bytes()).is_err());
    }

    #[test]
    fn marker_rejects_usize_overflow_and_trailing_data() {
        let valid = marker_json(PurchaseType::Rent);
        let overflow = format!("{}0", usize::MAX);
        let overflowing = valid.replace("\"title_id\":7", &format!("\"title_id\":{overflow}"));
        assert!(decode_marker(overflowing.as_bytes()).is_err());

        let trailing = format!("{valid} true");
        assert!(decode_marker(trailing.as_bytes()).is_err());
    }

    #[test]
    fn marker_requires_only_the_balance_affected_by_its_purchase_type() {
        let gift = marker_json(PurchaseType::RentGift);
        let paid = marker_json(PurchaseType::Rent);

        let gift_without_snapshot = gift.replace(
            "\"pre_mutation_title_gifts\":2",
            "\"pre_mutation_title_gifts\":null",
        );
        let gift_with_both = gift.replace(
            "\"pre_mutation_spendable_coin\":null",
            "\"pre_mutation_spendable_coin\":23",
        );
        let paid_without_snapshot = paid.replace(
            "\"pre_mutation_spendable_coin\":23",
            "\"pre_mutation_spendable_coin\":null",
        );
        let paid_with_both = paid.replace(
            "\"pre_mutation_title_gifts\":null",
            "\"pre_mutation_title_gifts\":2",
        );

        for invalid in [
            gift_without_snapshot,
            gift_with_both,
            paid_without_snapshot,
            paid_with_both,
        ] {
            assert!(
                decode_marker(invalid.as_bytes()).is_err(),
                "accepted {invalid}"
            );
        }
    }

    #[test]
    fn encoded_marker_contains_only_bounded_business_identity_and_snapshots() {
        let encoded = marker_json(PurchaseType::RentGift);
        for forbidden in [
            "Human-readable comic title",
            "Human-readable episode title",
            "access-token-value",
            "session-cookie-value",
            "provider-account-123",
        ] {
            assert!(!encoded.contains(forbidden), "marker leaked {forbidden}");
        }
        assert!(encoded.len() <= MAX_MARKER_BYTES);
    }

    #[test]
    fn safety_loading_waits_for_marker_authentication_scope_and_connectivity() {
        let mut commerce = Commerce::new();
        commerce.safety_changed(Authentication::Authenticated(scope()), Connectivity::Online);
        assert_eq!(commerce.state(), CommerceState::LoadingSafetyState);
        assert!(commerce
            .begin_quote(selection(), PurchaseType::Possession)
            .command
            .is_none());

        let effects = commerce.marker_loaded(None);
        assert!(effects.command.is_none());
        assert_eq!(commerce.state(), CommerceState::Idle);

        let mut reverse = Commerce::new();
        reverse.marker_loaded(None);
        assert_eq!(reverse.state(), CommerceState::LoadingSafetyState);
        reverse.safety_changed(Authentication::Authenticated(scope()), Connectivity::Online);
        assert_eq!(reverse.state(), CommerceState::Idle);
    }

    #[test]
    fn unsafe_authentication_connectivity_and_scope_states_cannot_quote() {
        let cases = [
            (Authentication::Unknown, Connectivity::Online),
            (Authentication::SignedOut, Connectivity::Online),
            (Authentication::Expired, Connectivity::Online),
            (
                Authentication::Authenticated(scope()),
                Connectivity::Unknown,
            ),
            (
                Authentication::Authenticated(scope()),
                Connectivity::Offline,
            ),
        ];

        for (authentication, connectivity) in cases {
            let mut commerce = Commerce::new();
            commerce.marker_loaded(None);
            commerce.safety_changed(authentication, connectivity);
            assert!(
                commerce
                    .begin_quote(selection(), PurchaseType::Possession)
                    .command
                    .is_none(),
                "unsafe case emitted a quote"
            );
        }
    }

    #[test]
    fn marker_save_acknowledgement_is_the_only_path_to_post() {
        let mut commerce = choosing(PurchaseType::Rent);
        assert!(matches!(
            commerce.choose(Action::Rent).command,
            Some(CommerceCommand::FetchQuote { .. })
        ));
        let save = commerce
            .quote_received(quote(), Some(23), Some(2), false)
            .command;
        assert!(matches!(save, Some(CommerceCommand::SaveMarker(_))));
        assert_eq!(commerce.state(), CommerceState::PersistingIntent);

        assert!(commerce.marker_saved("other.key").command.is_none());
        assert_eq!(commerce.state(), CommerceState::PersistingIntent);
        let post = commerce.marker_saved(MARKER_KEY).command;
        assert!(matches!(post, Some(CommerceCommand::Post(_))));
        assert_eq!(commerce.state(), CommerceState::Mutating);
        assert!(commerce.marker_saved(MARKER_KEY).command.is_none());
    }

    #[test]
    fn denied_save_restores_choice_without_posting() {
        let mut commerce = persisting(Action::Buy, PurchaseType::Possession);
        let effects = commerce.store_denied(StoreError::Unwritable);
        assert!(effects.command.is_none());
        assert_eq!(commerce.state(), CommerceState::Choosing);
    }

    #[test]
    fn explicit_rejection_requires_acknowledged_forget_before_another_quote() {
        let mut commerce = mutating(Action::Rent, PurchaseType::Rent);
        let forget = commerce.mutation_finished(PostOutcome::ExplicitRejection);
        assert!(matches!(
            forget.command,
            Some(CommerceCommand::ForgetMarker)
        ));
        assert_eq!(commerce.state(), CommerceState::ClearingIntent);
        assert!(commerce
            .begin_quote(selection(), PurchaseType::Possession)
            .command
            .is_none());

        assert!(commerce.marker_forgotten("other.key").command.is_none());
        assert_eq!(commerce.state(), CommerceState::ClearingIntent);
        commerce.marker_forgotten(MARKER_KEY);
        assert_eq!(commerce.state(), CommerceState::Idle);
        assert!(matches!(
            commerce
                .begin_quote(selection(), PurchaseType::Possession)
                .command,
            Some(CommerceCommand::FetchQuote { .. })
        ));
    }

    #[test]
    fn denied_forget_stays_locked_and_late_forgotten_cannot_unlock() {
        let mut commerce = mutating(Action::Rent, PurchaseType::Rent);
        commerce.mutation_finished(PostOutcome::ExplicitRejection);
        commerce.store_denied(StoreError::Unwritable);
        assert_eq!(commerce.state(), CommerceState::AcceptedButStale);
        assert!(commerce.marker_forgotten(MARKER_KEY).command.is_none());
        assert_eq!(commerce.state(), CommerceState::AcceptedButStale);
        assert!(commerce
            .begin_quote(selection(), PurchaseType::Rent)
            .command
            .is_none());
    }

    #[test]
    fn ambiguous_outcome_reconciles_all_authoritative_state_and_never_reposts() {
        let mut commerce = mutating(Action::Buy, PurchaseType::Possession);
        let effects = commerce.mutation_finished(PostOutcome::Ambiguous);
        assert!(matches!(
            effects.command,
            Some(CommerceCommand::RefreshContent(_))
        ));
        assert!(effects.refresh_wallet);
        assert!(effects.refresh_gifts);
        assert_eq!(commerce.state(), CommerceState::Reconciling);
        assert!(commerce.marker_saved(MARKER_KEY).command.is_none());

        commerce.reconciled(scope(), Reconciliation::Incomplete);
        assert_eq!(commerce.state(), CommerceState::AcceptedButStale);
        assert!(!commerce.marker_belongs_to_another_account());
        assert!(commerce.marker_saved(MARKER_KEY).command.is_none());
        assert!(matches!(
            commerce.refresh_status().command,
            Some(CommerceCommand::RefreshContent(_))
        ));
    }

    #[test]
    fn accepted_outcome_refreshes_only_the_affected_balance() {
        let mut paid = mutating(Action::Rent, PurchaseType::Rent);
        let paid_effects = paid.mutation_finished(PostOutcome::Accepted);
        assert!(paid_effects.refresh_wallet);
        assert!(!paid_effects.refresh_gifts);

        let mut gift = mutating(Action::UseGift, PurchaseType::RentGift);
        let gift_effects = gift.mutation_finished(PostOutcome::Accepted);
        assert!(!gift_effects.refresh_wallet);
        assert!(gift_effects.refresh_gifts);
    }

    #[test]
    fn reconciliation_and_forget_are_account_scoped() {
        let other_scope =
            AccountScope::from_bytes(b"ffeeddccbbaa99887766554433221100").expect("other scope");
        let mut commerce = mutating(Action::Rent, PurchaseType::Rent);
        commerce.mutation_finished(PostOutcome::Accepted);

        let effects = commerce.reconciled(other_scope, Reconciliation::Conclusive);
        assert!(effects.command.is_none());
        assert_eq!(commerce.state(), CommerceState::AcceptedButStale);
        assert!(commerce.marker_forgotten(MARKER_KEY).command.is_none());
    }

    #[test]
    fn same_account_restart_reconciles_but_different_account_never_queries_marker() {
        let bytes = encode_marker(&marker(PurchaseType::Rent)).expect("marker");
        let mut same = Commerce::new();
        same.marker_loaded(Some(&bytes));
        let effects =
            same.safety_changed(Authentication::Authenticated(scope()), Connectivity::Online);
        assert!(matches!(
            effects.command,
            Some(CommerceCommand::RefreshContent(_))
        ));
        assert!(effects.refresh_wallet);
        assert!(effects.refresh_gifts);
        assert_eq!(same.state(), CommerceState::Reconciling);
        assert!(!same.marker_belongs_to_another_account());

        let other_scope =
            AccountScope::from_bytes(b"ffeeddccbbaa99887766554433221100").expect("other scope");
        let mut different = Commerce::new();
        different.marker_loaded(Some(&bytes));
        let effects = different.safety_changed(
            Authentication::Authenticated(other_scope),
            Connectivity::Online,
        );
        assert!(effects.command.is_none());
        assert_eq!(different.state(), CommerceState::AcceptedButStale);
        assert!(different.marker_belongs_to_another_account());
        assert!(different.refresh_status().command.is_none());
        assert!(different.marker_forgotten(MARKER_KEY).command.is_none());
    }

    #[test]
    fn malformed_restart_marker_locks_without_reconcile_clear_or_post() {
        let mut commerce = Commerce::new();
        commerce.marker_loaded(Some(b"not a marker"));
        let effects =
            commerce.safety_changed(Authentication::Authenticated(scope()), Connectivity::Online);
        assert!(effects.command.is_none());
        assert_eq!(commerce.state(), CommerceState::AcceptedButStale);
        assert!(!commerce.marker_belongs_to_another_account());
        assert!(commerce.refresh_status().command.is_none());
        assert!(commerce.marker_saved(MARKER_KEY).command.is_none());
        assert!(commerce.marker_forgotten(MARKER_KEY).command.is_none());
    }

    #[test]
    fn every_durable_marker_state_refuses_a_second_post() {
        let bytes = encode_marker(&marker(PurchaseType::Rent)).expect("marker");

        let mut loading = Commerce::new();
        loading.marker_loaded(Some(&bytes));
        assert_eq!(loading.state(), CommerceState::LoadingSafetyState);
        assert!(loading.marker_saved(MARKER_KEY).command.is_none());

        let mut persisting = persisting(Action::Rent, PurchaseType::Rent);
        assert!(persisting.choose(Action::Rent).command.is_none());

        let mut posted = mutating(Action::Rent, PurchaseType::Rent);
        assert!(posted.marker_saved(MARKER_KEY).command.is_none());

        posted.mutation_finished(PostOutcome::Ambiguous);
        assert_eq!(posted.state(), CommerceState::Reconciling);
        assert!(posted.marker_saved(MARKER_KEY).command.is_none());

        let mut clearing = mutating(Action::Rent, PurchaseType::Rent);
        clearing.mutation_finished(PostOutcome::ExplicitRejection);
        assert_eq!(clearing.state(), CommerceState::ClearingIntent);
        assert!(clearing.marker_saved(MARKER_KEY).command.is_none());

        clearing.store_denied(StoreError::Unwritable);
        assert_eq!(clearing.state(), CommerceState::AcceptedButStale);
        assert!(clearing.marker_saved(MARKER_KEY).command.is_none());
    }

    fn quoted_with(
        quote: Quote,
        spendable_coin: Option<usize>,
        title_gifts: Option<usize>,
        active_rental: bool,
    ) -> Commerce {
        let mut commerce = ready();
        commerce.begin_quote(selection(), PurchaseType::Possession);
        commerce.quote_received(quote, spendable_coin, title_gifts, active_rental);
        commerce
    }

    #[test]
    fn quote_actions_have_exact_labels_and_cancel_is_always_enabled() {
        let commerce = quoted_with(quote(), Some(23), Some(2), false);
        let presentation = commerce.quote_presentation().expect("presentation");

        assert_eq!(presentation.control(Action::UseGift).label, "Use Gift");
        assert_eq!(presentation.control(Action::Rent).label, "Rent · 5 coins");
        assert_eq!(presentation.control(Action::Buy).label, "Buy · 9 coins");
        let cancel = presentation.control(Action::Cancel);
        assert_eq!(cancel.label, "Cancel");
        assert!(cancel.enabled);
        assert_eq!(cancel.disabled_reason, None);
    }

    #[test]
    fn unknown_and_insufficient_coin_have_distinct_disabled_reasons() {
        let unknown = quoted_with(quote(), None, Some(2), false);
        let presentation = unknown.quote_presentation().expect("unknown Coin quote");
        for action in [Action::Rent, Action::Buy] {
            let control = presentation.control(action);
            assert!(!control.enabled);
            assert_eq!(
                control.disabled_reason.as_deref(),
                Some("Coin balance unavailable")
            );
        }

        let mut insufficient = quoted_with(quote(), Some(4), Some(2), false);
        let presentation = insufficient
            .quote_presentation()
            .expect("insufficient Coin quote");
        assert_eq!(
            presentation
                .control(Action::Rent)
                .disabled_reason
                .as_deref(),
            Some("Need 5 coins")
        );
        assert_eq!(
            presentation.control(Action::Buy).disabled_reason.as_deref(),
            Some("Need 9 coins")
        );
        assert!(insufficient.choose(Action::Rent).command.is_none());
    }

    #[test]
    fn unknown_and_zero_gifts_have_distinct_disabled_reasons() {
        let unknown = quoted_with(quote(), Some(23), None, false);
        assert_eq!(
            unknown
                .quote_presentation()
                .expect("unknown Gift quote")
                .control(Action::UseGift)
                .disabled_reason
                .as_deref(),
            Some("Gift balance unavailable")
        );

        let mut zero = quoted_with(quote(), Some(23), Some(0), false);
        assert_eq!(
            zero.quote_presentation()
                .expect("zero Gift quote")
                .control(Action::UseGift)
                .disabled_reason
                .as_deref(),
            Some("No Gifts for this title")
        );
        assert!(zero.choose(Action::UseGift).command.is_none());
    }

    #[test]
    fn gift_remains_enabled_without_a_coin_balance() {
        let commerce = quoted_with(quote(), None, Some(1), false);
        let presentation = commerce.quote_presentation().expect("presentation");
        assert!(presentation.control(Action::UseGift).enabled);
        assert!(!presentation.control(Action::Rent).enabled);
        assert!(!presentation.control(Action::Buy).enabled);
    }

    #[test]
    fn unknown_coin_kind_disables_only_paid_actions() {
        let mut unknown_kind = quote();
        unknown_kind.coin_kind = "FUTURE".to_owned();
        let mut commerce = quoted_with(unknown_kind, Some(23), Some(1), false);
        let presentation = commerce.quote_presentation().expect("presentation");

        assert!(presentation.control(Action::UseGift).enabled);
        for action in [Action::Rent, Action::Buy] {
            assert_eq!(
                presentation.control(action).disabled_reason.as_deref(),
                Some("Coin payment unavailable")
            );
        }
        assert!(commerce.choose(Action::Rent).command.is_none());
    }

    #[test]
    fn remote_available_quote_reconciles_instead_of_offering_mutation() {
        let mut available = quote();
        available.is_available = true;
        let mut commerce = ready();
        commerce.begin_quote(selection(), PurchaseType::Possession);
        let effects = commerce.quote_received(available, Some(23), Some(2), false);

        assert!(matches!(
            effects.command,
            Some(CommerceCommand::RefreshContent(_))
        ));
        assert_eq!(commerce.state(), CommerceState::Reconciling);
        assert!(commerce.quote_presentation().is_none());
        assert!(commerce.choose(Action::Buy).command.is_none());
        assert!(commerce.marker_saved(MARKER_KEY).command.is_none());
    }

    #[test]
    fn changed_requote_replaces_presentation_without_saving_or_posting() {
        let mut commerce = quoted_with(quote(), Some(23), Some(2), false);
        let request = commerce.choose(Action::Rent);
        assert!(matches!(
            request.command,
            Some(CommerceCommand::FetchQuote {
                purchase: PurchaseType::Rent,
                ..
            })
        ));
        assert_eq!(commerce.state(), CommerceState::Requoting);

        let mut changed = quote();
        changed.rent_coin = 6;
        let effects = commerce.quote_received(changed, Some(23), Some(2), false);
        assert!(effects.command.is_none());
        assert_eq!(commerce.state(), CommerceState::Choosing);
        let presentation = commerce.quote_presentation().expect("changed presentation");
        assert!(presentation.quote_changed);
        assert_eq!(presentation.control(Action::Rent).label, "Rent · 6 coins");
        assert!(matches!(
            commerce.choose(Action::Rent).command,
            Some(CommerceCommand::FetchQuote {
                purchase: PurchaseType::Rent,
                ..
            })
        ));
        assert_eq!(commerce.state(), CommerceState::Requoting);
    }

    #[test]
    fn unchanged_requote_persists_intent_without_second_confirmation() {
        let mut commerce = quoted_with(quote(), Some(23), Some(2), false);
        commerce.choose(Action::Rent);
        let effects = commerce.quote_received(quote(), Some(23), Some(2), false);
        assert!(matches!(
            effects.command,
            Some(CommerceCommand::SaveMarker(_))
        ));
        assert_eq!(commerce.state(), CommerceState::PersistingIntent);
    }

    #[test]
    fn active_rental_exposes_no_purchase_action() {
        let mut commerce = quoted_with(quote(), Some(23), Some(2), true);
        let presentation = commerce.quote_presentation().expect("presentation");
        for action in [Action::UseGift, Action::Rent, Action::Buy] {
            let control = presentation.control(action);
            assert!(!control.enabled);
            assert_eq!(control.disabled_reason.as_deref(), Some("Active rental"));
        }
        assert!(commerce.choose(Action::Buy).command.is_none());
    }

    #[test]
    fn quote_identity_mismatch_fails_closed() {
        let mut commerce = ready();
        commerce.begin_quote(selection(), PurchaseType::Possession);
        let mut mismatched = quote();
        mismatched.episode_id += 1;
        let effects = commerce.quote_received(mismatched, Some(23), Some(2), false);
        assert!(effects.command.is_none());
        assert_eq!(commerce.state(), CommerceState::Idle);
        assert!(commerce.quote_presentation().is_none());
    }

    #[test]
    fn quote_failure_returns_idle_without_store_or_post_command() {
        let mut commerce = ready();
        commerce.begin_quote(selection(), PurchaseType::Possession);
        let effects = commerce.quote_failed();
        assert!(effects.command.is_none());
        assert_eq!(commerce.state(), CommerceState::Idle);

        let mut requote = quoted_with(quote(), Some(23), Some(2), false);
        requote.choose(Action::Rent);
        let effects = requote.quote_failed();
        assert!(effects.command.is_none());
        assert_eq!(requote.state(), CommerceState::Idle);
    }

    #[test]
    fn interrupted_marker_save_stays_locked_after_connectivity_returns() {
        let mut commerce = persisting(Action::Rent, PurchaseType::Rent);

        commerce.safety_changed(
            Authentication::Authenticated(scope()),
            Connectivity::Offline,
        );
        let effects =
            commerce.safety_changed(Authentication::Authenticated(scope()), Connectivity::Online);

        assert!(matches!(
            effects.command,
            Some(CommerceCommand::RefreshContent(_))
        ));
        assert_eq!(commerce.state(), CommerceState::Reconciling);
        assert!(commerce
            .begin_quote(selection(), PurchaseType::Rent)
            .command
            .is_none());
        assert!(commerce.marker_saved(MARKER_KEY).command.is_none());
    }

    #[test]
    fn unresolved_marker_is_inert_in_every_unsafe_account_state() {
        let encoded = encode_marker(&marker(PurchaseType::Rent)).expect("marker");
        let cases = [
            (Authentication::Unknown, Connectivity::Online),
            (Authentication::SignedOut, Connectivity::Online),
            (Authentication::Expired, Connectivity::Online),
            (
                Authentication::Authenticated(scope()),
                Connectivity::Unknown,
            ),
            (
                Authentication::Authenticated(scope()),
                Connectivity::Offline,
            ),
        ];

        for (authentication, connectivity) in cases {
            let mut commerce = Commerce::new();
            commerce.marker_loaded(Some(&encoded));
            let effects = commerce.safety_changed(authentication, connectivity);
            assert!(effects.command.is_none());
            assert!(commerce.refresh_status().command.is_none());
            assert!(commerce.marker_saved(MARKER_KEY).command.is_none());
            assert!(commerce.marker_forgotten(MARKER_KEY).command.is_none());
        }
    }

    #[test]
    fn denied_marker_load_locks_commerce_without_clear_or_post() {
        let mut commerce = Commerce::new();
        commerce.safety_changed(Authentication::Authenticated(scope()), Connectivity::Online);
        let effects = commerce.store_denied(StoreError::Unwritable);

        assert!(effects.command.is_none());
        assert_eq!(commerce.state(), CommerceState::AcceptedButStale);
        assert!(commerce
            .begin_quote(selection(), PurchaseType::Rent)
            .command
            .is_none());
        assert!(commerce.marker_saved(MARKER_KEY).command.is_none());
        assert!(commerce.marker_forgotten(MARKER_KEY).command.is_none());
    }

    #[test]
    fn accepted_reconciliation_waits_for_matching_forget_acknowledgement() {
        let mut commerce = mutating(Action::Rent, PurchaseType::Rent);
        commerce.mutation_finished(PostOutcome::Accepted);
        let effects = commerce.reconciled(scope(), Reconciliation::Conclusive);

        assert!(matches!(
            effects.command,
            Some(CommerceCommand::ForgetMarker)
        ));
        assert_eq!(commerce.state(), CommerceState::ClearingIntent);
        assert!(commerce
            .begin_quote(selection(), PurchaseType::Rent)
            .command
            .is_none());

        commerce.marker_forgotten(MARKER_KEY);
        assert_eq!(commerce.state(), CommerceState::Idle);
    }

    #[test]
    fn account_switch_invalidates_quotes_before_they_can_persist_intent() {
        let other_scope =
            AccountScope::from_bytes(b"ffeeddccbbaa99887766554433221100").expect("other scope");

        let mut choosing = choosing(PurchaseType::Rent);
        choosing.safety_changed(
            Authentication::Authenticated(other_scope),
            Connectivity::Online,
        );
        assert!(choosing.choose(Action::Rent).command.is_none());

        let mut quoting = ready();
        quoting.begin_quote(selection(), PurchaseType::Rent);
        quoting.safety_changed(
            Authentication::Authenticated(other_scope),
            Connectivity::Online,
        );
        assert!(quoting
            .quote_received(quote(), Some(23), Some(2), false)
            .command
            .is_none());
        assert!(quoting.choose(Action::Rent).command.is_none());
    }

    #[test]
    fn initial_matching_quote_still_requotes_immediately_before_buy() {
        let mut commerce = quoted_with(quote(), Some(23), Some(2), false);
        let effects = commerce.choose(Action::Buy);

        assert!(matches!(
            effects.command,
            Some(CommerceCommand::FetchQuote {
                purchase: PurchaseType::Possession,
                ..
            })
        ));
        assert_eq!(commerce.state(), CommerceState::Requoting);
    }

    #[test]
    fn account_switch_invalidates_markerless_reconciliation_and_ignores_late_completion() {
        let other_scope =
            AccountScope::from_bytes(b"ffeeddccbbaa99887766554433221100").expect("other scope");
        let mut available = quote();
        available.is_available = true;
        let mut commerce = ready();
        commerce.begin_quote(selection(), PurchaseType::Possession);
        commerce.quote_received(available, Some(23), Some(2), false);
        assert_eq!(commerce.state(), CommerceState::Reconciling);

        commerce.safety_changed(
            Authentication::Authenticated(other_scope),
            Connectivity::Online,
        );
        assert_eq!(commerce.state(), CommerceState::Idle);

        let late = commerce.reconciled(scope(), Reconciliation::Conclusive);
        assert_eq!(late, CommerceEffects::default());
        assert_eq!(commerce.state(), CommerceState::Idle);
    }

    #[test]
    fn failed_markerless_reconciliation_returns_to_safe_idle() {
        let mut available = quote();
        available.is_available = true;
        let mut commerce = ready();
        commerce.begin_quote(selection(), PurchaseType::Possession);
        commerce.quote_received(available, Some(23), Some(2), false);

        let effects = commerce.reconciled(scope(), Reconciliation::Incomplete);
        assert!(effects.command.is_none());
        assert_eq!(commerce.state(), CommerceState::Idle);
        assert!(matches!(
            commerce
                .begin_quote(selection(), PurchaseType::Possession)
                .command,
            Some(CommerceCommand::FetchQuote { .. })
        ));
    }
}
