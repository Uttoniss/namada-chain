//! Extend [events](Event) with additional fields.

pub mod dynamic;

use std::fmt::Display;
use std::marker::PhantomData;
use std::ops::ControlFlow;
use std::str::FromStr;

use namada_core::collections::HashMap;
use namada_core::hash::Hash;
use namada_core::storage::{BlockHeight, TxIndex};

use super::*;

impl Event {
    /// Check if this [`Event`] has a subset of the keys and values
    /// in `attrs`.
    #[inline]
    pub fn has_subset_of_attrs<A: AttributesMap>(&self, attrs: &A) -> bool {
        attrs.iter_attributes().all(|(key, value)| {
            match self.attributes.get(key) {
                Some(v) => v == value,
                None => false,
            }
        })
    }

    /// Get the raw string value corresponding to a given attribute, if it
    /// exists.
    #[inline]
    pub fn raw_read_attribute<'value, DATA>(&self) -> Option<&str>
    where
        DATA: RawReadFromEventAttributes<'value>,
    {
        DATA::raw_read_opt_from_event_attributes(&self.attributes)
    }

    /// Get the value corresponding to a given attribute.
    #[inline]
    pub fn read_attribute<'value, DATA>(
        &self,
    ) -> Result<<DATA as ReadFromEventAttributes<'value>>::Value, EventError>
    where
        DATA: ReadFromEventAttributes<'value>,
    {
        DATA::read_from_event_attributes(&self.attributes)
    }

    /// Get the value corresponding to a given attribute, if it exists.
    #[inline]
    pub fn read_attribute_opt<'value, DATA>(
        &self,
    ) -> Result<
        Option<<DATA as ReadFromEventAttributes<'value>>::Value>,
        EventError,
    >
    where
        DATA: ReadFromEventAttributes<'value>,
    {
        DATA::read_opt_from_event_attributes(&self.attributes)
    }

    /// Check if a certain attribute is present in the event.
    #[inline]
    pub fn has_attribute<'value, DATA>(&self) -> bool
    where
        DATA: RawReadFromEventAttributes<'value>,
    {
        DATA::check_if_present_in(&self.attributes)
    }

    /// Extend this [`Event`] with additional data.
    #[inline]
    pub fn extend<DATA>(&mut self, data: DATA) -> &mut Self
    where
        DATA: ExtendEvent,
    {
        data.extend_event(self);
        self
    }
}

/// Map of event attributes.
pub trait AttributesMap {
    /// Insert a new attribute.
    fn insert_attribute<K, V>(&mut self, key: K, value: V)
    where
        K: Into<String>,
        V: Into<String>;

    /// Retrieve an attribute.
    fn retrieve_attribute(&self, key: &str) -> Option<&str>;

    /// Check for the existence of an attribute.
    fn is_attribute(&self, key: &str) -> bool;

    /// Iterate over all the key value pairs.
    fn iter_attributes(&self) -> impl Iterator<Item = (&str, &str)>;
}

impl AttributesMap for HashMap<String, String> {
    #[inline]
    fn insert_attribute<K, V>(&mut self, key: K, value: V)
    where
        K: Into<String>,
        V: Into<String>,
    {
        self.insert(key.into(), value.into());
    }

    #[inline]
    fn retrieve_attribute(&self, key: &str) -> Option<&str> {
        self.get(key).map(String::as_ref)
    }

    #[inline]
    fn is_attribute(&self, key: &str) -> bool {
        self.contains_key(key)
    }

    #[inline]
    fn iter_attributes(&self) -> impl Iterator<Item = (&str, &str)> {
        self.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

impl AttributesMap for BTreeMap<String, String> {
    #[inline]
    fn insert_attribute<K, V>(&mut self, key: K, value: V)
    where
        K: Into<String>,
        V: Into<String>,
    {
        self.insert(key.into(), value.into());
    }

    #[inline]
    fn retrieve_attribute(&self, key: &str) -> Option<&str> {
        self.get(key).map(String::as_ref)
    }

    #[inline]
    fn is_attribute(&self, key: &str) -> bool {
        self.contains_key(key)
    }

    #[inline]
    fn iter_attributes(&self) -> impl Iterator<Item = (&str, &str)> {
        self.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

impl AttributesMap for Vec<namada_core::tendermint::abci::EventAttribute> {
    #[inline]
    fn insert_attribute<K, V>(&mut self, key: K, value: V)
    where
        K: Into<String>,
        V: Into<String>,
    {
        self.push(namada_core::tendermint::abci::EventAttribute {
            key: key.into(),
            value: value.into(),
            index: true,
        });
    }

    #[inline]
    fn retrieve_attribute(&self, key: &str) -> Option<&str> {
        self.iter().find_map(|attr| {
            if attr.key == key {
                Some(attr.value.as_str())
            } else {
                None
            }
        })
    }

    #[inline]
    fn is_attribute(&self, key: &str) -> bool {
        self.iter().any(|attr| attr.key == key)
    }

    #[inline]
    fn iter_attributes(&self) -> impl Iterator<Item = (&str, &str)> {
        self.iter()
            .map(|attr| (attr.key.as_str(), attr.value.as_str()))
    }
}

impl AttributesMap
    for Vec<namada_core::tendermint_proto::v0_37::abci::EventAttribute>
{
    #[inline]
    fn insert_attribute<K, V>(&mut self, key: K, value: V)
    where
        K: Into<String>,
        V: Into<String>,
    {
        self.push(namada_core::tendermint_proto::v0_37::abci::EventAttribute {
            key: key.into(),
            value: value.into(),
            index: true,
        });
    }

    #[inline]
    fn retrieve_attribute(&self, key: &str) -> Option<&str> {
        self.iter().find_map(|attr| {
            if attr.key == key {
                Some(attr.value.as_str())
            } else {
                None
            }
        })
    }

    #[inline]
    fn is_attribute(&self, key: &str) -> bool {
        self.iter().any(|attr| attr.key == key)
    }

    #[inline]
    fn iter_attributes(&self) -> impl Iterator<Item = (&str, &str)> {
        self.iter()
            .map(|attr| (attr.key.as_str(), attr.value.as_str()))
    }
}

/// Provides event composition routines.
pub trait ComposeEvent {
    /// Compose an [event](Event) with new data.
    fn with<NEW>(self, data: NEW) -> CompositeEvent<NEW, Self>
    where
        Self: Sized;
}

impl<E> ComposeEvent for E
where
    E: Into<Event>,
{
    #[inline(always)]
    fn with<NEW>(self, data: NEW) -> CompositeEvent<NEW, E> {
        CompositeEvent::new(self, data)
    }
}

/// Event composed of various other event extensions.
#[derive(Clone, Debug)]
pub struct CompositeEvent<DATA, E> {
    base_event: E,
    data: DATA,
}

impl<E, DATA> CompositeEvent<DATA, E> {
    /// Create a new composed event.
    pub const fn new(base_event: E, data: DATA) -> Self {
        Self { base_event, data }
    }
}

impl<E, DATA> From<CompositeEvent<DATA, E>> for Event
where
    E: Into<Event>,
    DATA: ExtendEvent,
{
    #[inline]
    fn from(composite: CompositeEvent<DATA, E>) -> Event {
        let CompositeEvent { base_event, data } = composite;

        let mut base_event = base_event.into();
        data.extend_event(&mut base_event);

        base_event
    }
}

impl<E, DATA> EventToEmit for CompositeEvent<DATA, E>
where
    E: EventToEmit,
    DATA: ExtendEvent,
{
    const DOMAIN: &'static str = E::DOMAIN;
}

/// Extend an [`AttributesMap`] implementation with the ability
/// to add new attributes from domain types.
pub trait ExtendAttributesMap: Sized {
    /// Insert a new attribute into a map of event attributes.
    fn with_attribute<DATA>(&mut self, data: DATA) -> &mut Self
    where
        DATA: ExtendEventAttributes;
}

impl<A: AttributesMap> ExtendAttributesMap for A {
    #[inline(always)]
    fn with_attribute<DATA>(&mut self, data: DATA) -> &mut Self
    where
        DATA: ExtendEventAttributes,
    {
        data.extend_event_attributes(self);
        self
    }
}

/// Represents an entry in the attributes of an [`Event`].
pub trait EventAttributeEntry<'a> {
    /// Key to read or write and event attribute to.
    const KEY: &'static str;

    /// Data to be stored in the given `KEY`.
    type Value;

    /// Identical to [`Self::Value`], with the exception that this
    /// should be an owned variant of that type.
    type ValueOwned;

    /// Return the data to be stored in the given `KEY`.
    fn into_value(self) -> Self::Value;
}

/// Extend an [event](Event) with additional attributes.
pub trait ExtendEventAttributes {
    /// Add additional attributes to some `event`.
    fn extend_event_attributes<A>(self, attributes: &mut A)
    where
        A: AttributesMap;
}

impl<'value, DATA> ExtendEventAttributes for DATA
where
    DATA: EventAttributeEntry<'value>,
    DATA::Value: ToString,
{
    #[inline]
    fn extend_event_attributes<A>(self, attributes: &mut A)
    where
        A: AttributesMap,
    {
        attributes.insert_attribute(
            DATA::KEY.to_string(),
            self.into_value().to_string(),
        );
    }
}

/// Read an attribute from an [event](Event)'s attributes.
pub trait ReadFromEventAttributes<'value> {
    /// The attribute to be read.
    type Value;

    /// Read an attribute from the provided event attributes.
    fn read_opt_from_event_attributes<A>(
        attributes: &A,
    ) -> Result<Option<Self::Value>, EventError>
    where
        A: AttributesMap;

    /// Read an attribute from the provided event attributes.
    fn read_from_event_attributes<A>(
        attributes: &A,
    ) -> Result<Self::Value, EventError>
    where
        A: AttributesMap;
}

// NB: some domain specific types take references instead of owned
// values as arguments, so we must decode into the owned counterparts
// of these types... hence the trait spaghetti
impl<'value, DATA> ReadFromEventAttributes<'value> for DATA
where
    DATA: EventAttributeEntry<'value>,
    <DATA as EventAttributeEntry<'value>>::ValueOwned: FromStr,
    <<DATA as EventAttributeEntry<'value>>::ValueOwned as FromStr>::Err:
        Display,
{
    type Value = <DATA as EventAttributeEntry<'value>>::ValueOwned;

    #[inline]
    fn read_opt_from_event_attributes<A>(
        attributes: &A,
    ) -> Result<Option<Self::Value>, EventError>
    where
        A: AttributesMap,
    {
        attributes
            .retrieve_attribute(DATA::KEY)
            .map(|encoded_value| {
                encoded_value.parse().map_err(
                    |err: <Self::Value as FromStr>::Err| {
                        EventError::AttributeEncoding(err.to_string())
                    },
                )
            })
            .transpose()
    }

    #[inline]
    fn read_from_event_attributes<A>(
        attributes: &A,
    ) -> Result<Self::Value, EventError>
    where
        A: AttributesMap,
    {
        Self::read_opt_from_event_attributes(attributes)?.ok_or_else(|| {
            EventError::MissingAttribute(
                <Self as EventAttributeEntry<'value>>::KEY.to_string(),
            )
        })
    }
}

/// Read a raw (string encoded) attribute from an [event](Event)'s attributes.
pub trait RawReadFromEventAttributes<'value> {
    /// Check if the associated attribute is present in the provided event
    /// attributes.
    fn check_if_present_in<A>(attributes: &A) -> bool
    where
        A: AttributesMap;

    /// Read a string encoded attribute from the provided event attributes.
    fn raw_read_opt_from_event_attributes<A>(attributes: &A) -> Option<&str>
    where
        A: AttributesMap;

    /// Read a string encoded attribute from the provided event attributes.
    fn raw_read_from_event_attributes<A>(
        attributes: &A,
    ) -> Result<&str, EventError>
    where
        A: AttributesMap;
}

impl<'value, DATA> RawReadFromEventAttributes<'value> for DATA
where
    DATA: EventAttributeEntry<'value>,
{
    #[inline]
    fn check_if_present_in<A>(attributes: &A) -> bool
    where
        A: AttributesMap,
    {
        attributes.is_attribute(DATA::KEY)
    }

    #[inline]
    fn raw_read_opt_from_event_attributes<A>(attributes: &A) -> Option<&str>
    where
        A: AttributesMap,
    {
        attributes.retrieve_attribute(DATA::KEY)
    }

    #[inline]
    fn raw_read_from_event_attributes<A>(
        attributes: &A,
    ) -> Result<&str, EventError>
    where
        A: AttributesMap,
    {
        Self::raw_read_opt_from_event_attributes(attributes).ok_or_else(|| {
            EventError::MissingAttribute(
                <Self as EventAttributeEntry<'value>>::KEY.to_string(),
            )
        })
    }
}

/// Extend an [event](Event) with additional data.
pub trait ExtendEvent {
    /// Add additional data to the specified `event`.
    fn extend_event(self, event: &mut Event);
}

impl<E: ExtendEventAttributes> ExtendEvent for E {
    #[inline]
    fn extend_event(self, event: &mut Event) {
        self.extend_event_attributes(&mut event.attributes);
    }
}

/// Extend an [`Event`] with block height information.
pub struct Height(pub BlockHeight);

impl EventAttributeEntry<'static> for Height {
    type Value = BlockHeight;
    type ValueOwned = Self::Value;

    const KEY: &'static str = "height";

    fn into_value(self) -> Self::Value {
        self.0
    }
}

/// Extend an [`Event`] with transaction hash information.
pub struct TxHash(pub Hash);

impl EventAttributeEntry<'static> for TxHash {
    type Value = Hash;
    type ValueOwned = Self::Value;

    const KEY: &'static str = "hash";

    fn into_value(self) -> Self::Value {
        self.0
    }
}

/// Extend an [`Event`] with log data.
pub struct Log(pub String);

impl EventAttributeEntry<'static> for Log {
    type Value = String;
    type ValueOwned = Self::Value;

    const KEY: &'static str = "log";

    fn into_value(self) -> Self::Value {
        self.0
    }
}

/// Extend an [`Event`] with info data.
pub struct Info(pub String);

impl EventAttributeEntry<'static> for Info {
    type Value = String;
    type ValueOwned = Self::Value;

    const KEY: &'static str = "info";

    fn into_value(self) -> Self::Value {
        self.0
    }
}

/// Extend an [`Event`] with `is_valid_masp_tx` data.
pub struct ValidMaspTx(pub TxIndex);

impl EventAttributeEntry<'static> for ValidMaspTx {
    type Value = TxIndex;
    type ValueOwned = Self::Value;

    const KEY: &'static str = "is_valid_masp_tx";

    fn into_value(self) -> Self::Value {
        self.0
    }
}

/// Extend an [`Event`] with success data.
pub struct Success(pub bool);

impl EventAttributeEntry<'static> for Success {
    type Value = bool;
    type ValueOwned = Self::Value;

    const KEY: &'static str = "success";

    fn into_value(self) -> Self::Value {
        self.0
    }
}

/// Extend an [`Event`] with a new domain.
pub struct Domain<E>(PhantomData<E>);

/// Build a new [`Domain`] to extend an [event](Event) with.
pub const fn event_domain_of<E: EventToEmit>() -> Domain<E> {
    Domain(PhantomData)
}

/// Parsed domain of some [event](Event).
pub struct ParsedDomain<E> {
    domain: String,
    _marker: PhantomData<E>,
}

impl<E> ParsedDomain<E> {
    /// Return the inner domain as a [`String`].
    #[inline]
    pub fn into_inner(self) -> String {
        self.domain
    }
}

impl<E> From<ParsedDomain<E>> for String {
    #[inline]
    fn from(parsed_domain: ParsedDomain<E>) -> String {
        parsed_domain.into_inner()
    }
}

impl<E> FromStr for ParsedDomain<E>
where
    E: EventToEmit,
{
    type Err = EventError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == E::DOMAIN {
            Ok(Self {
                domain: s.to_owned(),
                _marker: PhantomData,
            })
        } else {
            Err(EventError::InvalidDomain(format!(
                "Expected {:?}, but found {s:?}",
                E::DOMAIN
            )))
        }
    }
}

impl<E> EventAttributeEntry<'static> for Domain<E>
where
    E: EventToEmit,
{
    type Value = &'static str;
    type ValueOwned = ParsedDomain<E>;

    const KEY: &'static str = "event-domain";

    fn into_value(self) -> Self::Value {
        E::DOMAIN
    }
}

/// Extend an [`Event`] with metadata pertaining to its origin
/// in the source code.
pub struct Origin {
    #[doc(hidden)]
    pub __origin: &'static str,
}

#[macro_export]
macro_rules! event_origin {
    () => {
        $crate::extend::Origin {
            __origin: ::konst::string::str_concat!(&[
                ::core::env!("CARGO_CRATE_NAME"),
                "-",
                ::core::env!("CARGO_PKG_VERSION"),
                ":",
                ::core::file!(),
                ":",
                ::core::line!()
            ]),
        }
    };
}

impl EventAttributeEntry<'static> for Origin {
    type Value = &'static str;
    type ValueOwned = String;

    const KEY: &'static str = "event-origin";

    fn into_value(self) -> Self::Value {
        self.__origin
    }
}

/// Extend an [`Event`] with the given closure.
pub struct Closure<F>(pub F);

impl<F> ExtendEvent for Closure<F>
where
    F: FnOnce(&mut Event),
{
    #[inline]
    fn extend_event(self, event: &mut Event) {
        let Self(closure) = self;
        closure(event);
    }
}

#[cfg(test)]
mod event_composition_tests {
    use super::*;

    struct DummyEvent;

    impl From<DummyEvent> for Event {
        fn from(_: DummyEvent) -> Event {
            Event::new(
                EventTypeBuilder::new_of::<DummyEvent>()
                    .with_segment("event")
                    .build(),
                EventLevel::Tx,
            )
        }
    }

    impl EventToEmit for DummyEvent {
        const DOMAIN: &'static str = "dummy";
    }

    #[test]
    fn test_event_height_parse() {
        let event: Event = DummyEvent.with(Height(BlockHeight(300))).into();

        let height = event.raw_read_attribute::<Height>().unwrap();
        assert_eq!(height, "300");
        assert_eq!(height.parse::<u64>().unwrap(), 300u64);

        let height = event.read_attribute::<Height>().unwrap();
        assert_eq!(height, BlockHeight(300));
    }

    #[test]
    fn test_event_compose_basic() {
        let expected_attrs = {
            let mut attrs = BTreeMap::new();
            attrs.insert("log".to_string(), "this is sparta!".to_string());
            attrs.insert("height".to_string(), "300".to_string());
            attrs.insert("hash".to_string(), Hash::default().to_string());
            attrs
        };

        let base_event: Event = DummyEvent
            .with(Log("this is sparta!".to_string()))
            .with(Height(300.into()))
            .with(TxHash(Hash::default()))
            .into();

        assert_eq!(base_event.attributes, expected_attrs);
    }

    #[test]
    fn test_event_compose_repeated() {
        let expected_attrs = {
            let mut attrs = BTreeMap::new();
            attrs.insert("log".to_string(), "dejavu".to_string());
            attrs
        };

        let base_event: Event = DummyEvent
            .with(Log("dejavu".to_string()))
            .with(Log("dejavu".to_string()))
            .with(Log("dejavu".to_string()))
            .into();

        assert_eq!(base_event.attributes, expected_attrs);
    }

    #[test]
    fn test_event_compose_last_one_kept() {
        let expected_attrs = {
            let mut attrs = BTreeMap::new();
            attrs.insert("log".to_string(), "last".to_string());
            attrs
        };

        let base_event: Event = DummyEvent
            .with(Log("fist".to_string()))
            .with(Log("second".to_string()))
            .with(Log("last".to_string()))
            .into();

        assert_eq!(base_event.attributes, expected_attrs);
    }
}
