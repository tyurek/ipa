use std::fmt::{Display, Formatter};

use super::{Epoch, KeyIdentifier};

const DOMAIN: &str = "private-attribution";

/// Represents the [`info`] part of the receiver context, that is: application specific data
/// for each encryption.
///
/// IPA uses key identifier, key event epoch, helper and match key provider origins, and
/// site registrable domain to authenticate the encryption of a match key.
/// It is not guaranteed that the same receiver can be used for anything else.
///
/// [`info`]: https://www.rfc-editor.org/rfc/rfc9180.html#name-creating-the-encryption-con
#[derive(Clone)]
pub struct Info<'a> {
    pub(super) key_id: KeyIdentifier,
    pub(super) epoch: Epoch,
    pub(super) match_key_provider_origin: &'a str,
    pub(super) helper_origin: &'a str,
    pub(super) site_domain: &'a str,
}

#[derive(Debug)]
pub struct NonAsciiStringError<'a> {
    input: &'a str,
}

impl Display for NonAsciiStringError<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "string contains non-ascii symbols: {}", self.input)
    }
}

impl<'a> From<&'a str> for NonAsciiStringError<'a> {
    fn from(input: &'a str) -> Self {
        Self { input }
    }
}

impl<'a> Info<'a> {
    /// Creates a new instance.
    ///
    /// ## Errors
    /// if helper or site origin is not a valid ASCII string.
    pub fn new(
        key_id: KeyIdentifier,
        epoch: Epoch,
        match_key_provider_origin: &'a str,
        helper_origin: &'a str,
        site_registrable_domain: &'a str,
    ) -> Result<Self, NonAsciiStringError<'a>> {
        if !match_key_provider_origin.is_ascii() {
            return Err(match_key_provider_origin.into());
        }

        if !helper_origin.is_ascii() {
            return Err(helper_origin.into());
        }

        if !site_registrable_domain.is_ascii() {
            return Err(site_registrable_domain.into());
        }

        Ok(Self {
            key_id,
            epoch,
            match_key_provider_origin,
            helper_origin,
            site_registrable_domain,
        })
    }

    /// Converts this instance into an owned byte slice that can further be used to create HPKE
    /// sender or receiver context.
    pub(super) fn into_bytes(self) -> Box<[u8]> {
        let info_len = DOMAIN.len()
            + self.match_key_provider_origin.len()
            + self.helper_origin.len()
            + self.site_registrable_domain.len()
            + 4 // account for 4 delimiters
            + std::mem::size_of_val(&self.key_id)
            + std::mem::size_of_val(&self.epoch);
        let mut r = Vec::with_capacity(info_len);

        r.extend_from_slice(DOMAIN.as_bytes());
        r.push(0);
        r.extend_from_slice(self.match_key_provider_origin.as_bytes());
        r.push(0);
        r.extend_from_slice(self.helper_origin.as_bytes());
        r.push(0);
        r.extend_from_slice(self.site_registrable_domain.as_bytes());
        r.push(0);

        r.push(self.key_id);
        // Spec dictates epoch to be encoded in BE
        r.extend_from_slice(&self.epoch.to_be_bytes());

        debug_assert_eq!(r.len(), info_len, "HPKE Info length estimation is incorrect and leads to extra allocation or wasted memory");

        r.into_boxed_slice()
    }
}
