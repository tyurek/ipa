use ipa_step_derive::CompactStep;

#[derive(CompactStep)]
pub enum BitOpStep {
    #[step(count = 512)] // TODO: add child steps for multiplication
    Bit(usize),
}

impl From<i32> for BitOpStep {
    fn from(v: i32) -> Self {
        Self::Bit(usize::try_from(v).unwrap())
    }
}

impl From<u32> for BitOpStep {
    fn from(v: u32) -> Self {
        Self::Bit(usize::try_from(v).unwrap())
    }
}

impl From<usize> for BitOpStep {
    fn from(v: usize) -> Self {
        Self::Bit(v)
    }
}

#[derive(CompactStep)]
pub(crate) enum SolvedBitsStep {
    RandomBits,
    IsPLessThanB,
    RevealC,
}

#[derive(CompactStep)]
pub(crate) enum ComparisonStep {
    PrefixOr,
    DotProduct,
}

#[derive(CompactStep)]
pub(crate) enum BitwiseLessThanStep {
    CheckTrimmed,
    CheckIfAnyOnes,
    LeadingOnesOrRest,
    CheckIfAllOnes,
    CheckLeastSignificantBits,
    AllOnesAndFinalBits,
}

#[derive(CompactStep)]
pub(crate) enum GreaterThanConstantStep {
    Reveal,
    CompareLo,
    CompareHi,
    And,
}

#[derive(CompactStep)]
pub(crate) enum BoolAndStep {
    #[step(count = 8)] // keep in sync with MAX_BITS defined inside and.rs
    Bit(usize),
}