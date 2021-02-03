//! # Faucet Module
//!
//! The Faucet module allows a root key (sudo) to add accounts (faucets) that are eligible
//! to drip free tokens to other accounts (recipients).

// TODO refactor sudo to generic account + add 'created' to FaucetSettings so we can check owner

#![cfg_attr(not(feature = "std"), no_std)]

use codec::{Decode, Encode};
use frame_support::{
    decl_error, decl_event, decl_module, decl_storage, dispatch::{DispatchError, DispatchResult},
    ensure,
    traits::{Currency, ExistenceRequirement, Get},
    weights::Pays,
};
use frame_system::{self as system, ensure_root, ensure_signed};
use pallet_sudo::Module as SudoModule;
use sp_runtime::RuntimeDebug;
use sp_runtime::traits::{Saturating, Zero};
use sp_std::{
    collections::btree_set::BTreeSet,
    iter::FromIterator,
    prelude::*,
};

#[cfg(test)]
mod mock;

#[cfg(test)]
mod tests;

type DropId = u64;

#[derive(Encode, Decode, Clone, Eq, PartialEq, RuntimeDebug)]
pub struct Drop<T: Trait> {
    id: DropId,
    last_drop_at: T::BlockNumber,
    total_dropped: BalanceOf<T>,
}

#[derive(Encode, Decode, Clone, Eq, PartialEq, RuntimeDebug)]
pub struct FaucetSettings<BlockNumber, Balance> {
    period: Option<BlockNumber>,
    period_limit: Balance,
    drop_limit: Balance,
}

#[derive(Encode, Decode, Clone, Eq, PartialEq, RuntimeDebug)]
pub struct FaucetSettingsUpdate<BlockNumber, Balance> {
    period: Option<Option<BlockNumber>>,
    period_limit: Option<Balance>,
    drop_limit: Option<Balance>,
}

type BalanceOf<T> = <<T as Trait>::Currency as Currency<<T as system::Trait>::AccountId>>::Balance;

/// The pallet's configuration trait.
pub trait Trait: system::Trait + pallet_sudo::Trait {
    /// The overarching event type.
    type Event: From<Event<Self>> + Into<<Self as system::Trait>::Event>;

    type Currency: Currency<Self::AccountId>;
}

decl_storage! {
	trait Store for Module<T: Trait> as FaucetModule {
		pub NextDropId get(fn next_drop_id): DropId = 1;

		pub DropById get(fn drop_by_id):
			map hasher(twox_64_concat) DropId
			=> Option<Drop<T>>;

		pub DropIdByRecipient get(fn drop_id_by_recipient):
			map hasher(twox_64_concat) T::AccountId
			=> Option<DropId>;

		pub SettingsByFaucet get(fn settings_by_faucet):
			map hasher(twox_64_concat) T::AccountId
			=> Option<FaucetSettings<T::BlockNumber, BalanceOf<T>>>;

	    pub TotalFaucetDropsByAccount get(fn total_faucet_drops_by_account): double_map
	        hasher(twox_64_concat) T::AccountId,    // Faucet account
	        hasher(twox_64_concat) T::AccountId     // User account
	        => BalanceOf<T>;
	}
}

decl_event!(
	pub enum Event<T> where
		AccountId = <T as system::Trait>::AccountId,
		Balance = BalanceOf<T>
	{
		FaucetAdded(AccountId),
		FaucetUpdated(AccountId),
		FaucetsRemoved(Vec<AccountId>),
		Dropped(
			AccountId, // faucet
			AccountId, // recipient
			Balance // amount dropped
		),
	}
);

// The pallet's errors
decl_error! {
	pub enum Error for Module<T: Trait> {
		FaucetNotFound,
		FaucetAlreadyAdded,
		FaucetLimitReached,
		NoFaucetsProvided,
		NoFreeBalanceOnAccount,
		NothingToUpdate,
		ZeroAmount,
		DropAmountLimit,
	}
}

// The pallet's dispatchable functions.
decl_module! {
    /// The module declaration.
    pub struct Module<T: Trait> for enum Call where origin: T::Origin {
        // Initializing errors
        type Error = Error<T>;

        // Initializing events
        fn deposit_event() = default;

        #[weight = T::DbWeight::get().reads_writes(1, 1) + 50_000]
        pub fn add_faucet(
            origin,
            faucet: T::AccountId,
            settings: FaucetSettings<T::BlockNumber, BalanceOf<T>>
        ) -> DispatchResult {
            ensure_root(origin)?;

            ensure!(
                Self::require_faucet_settings(&faucet).is_err(),
                Error::<T>::FaucetAlreadyAdded
            );

            ensure!(
                T::Currency::free_balance(&faucet) >= T::Currency::minimum_balance(),
                Error::<T>::NoFreeBalanceOnAccount
            );

            SettingsByFaucet::<T>::insert(faucet.clone(), settings);

            Self::deposit_event(RawEvent::FaucetAdded(faucet));
            Ok(())
        }

        #[weight = T::DbWeight::get().reads_writes(1, 1) + 20_000]
        pub fn update_faucet(
            origin,
            faucet: T::AccountId,
            update: FaucetSettingsUpdate<T::BlockNumber, BalanceOf<T>>
        ) -> DispatchResult {
            ensure_root(origin)?;

            let has_updates =
                update.period.is_some() ||
                update.period_limit.is_some() ||
                update.drop_limit.is_some();

            ensure!(has_updates, Error::<T>::NothingToUpdate);

            let mut settings = Self::require_faucet_settings(&faucet)?;

            // `true` if there is at least one updated field.
            let mut should_update = false;

            if let Some(period) = update.period {
                if period != settings.period {
                    settings.period = period;
                    should_update = true;
                }
            }

            if let Some(period_limit) = update.period_limit {
                if period_limit != settings.period_limit {
                    settings.period_limit = period_limit;
                    should_update = true;
                }
            }

            if let Some(drop_limit) = update.drop_limit {
                if drop_limit != settings.drop_limit {
                    settings.drop_limit = drop_limit;
                    should_update = true;
                }
            }

            if should_update {
                SettingsByFaucet::<T>::insert(faucet.clone(), settings);
                Self::deposit_event(RawEvent::FaucetUpdated(faucet));
                return Ok(());
            }
            Err(Error::<T>::NothingToUpdate.into())
        }

        #[weight = T::DbWeight::get().reads_writes(0, 1) + 10_000 + 5_000 * faucets.len() as u64]
        pub fn remove_faucets(
            origin,
            faucets: Vec<T::AccountId>
        ) -> DispatchResult {
            ensure_root(origin)?;
            let root_key = SudoModule::<T>::key();

            ensure!(faucets.len() != Zero::zero(), Error::<T>::NoFaucetsProvided);

            let unique_faucets = BTreeSet::from_iter(faucets.iter());
            for faucet in unique_faucets.iter() {
                if Self::require_faucet_settings(faucet).is_ok() {
                    T::Currency::transfer(
                        faucet,
                        &root_key,
                        T::Currency::free_balance(faucet),
                        ExistenceRequirement::AllowDeath
                    )?;

                    SettingsByFaucet::<T>::remove(faucet);
                }
            }

            Self::deposit_event(RawEvent::FaucetsRemoved(faucets));
            Ok(())
        }

        #[weight = (
            T::DbWeight::get().reads_writes(6, 4) + 50_000,
            Pays::No
        )]
        pub fn drip(
            origin, // faucet account
            amount: BalanceOf<T>,
            recipient: T::AccountId
        ) -> DispatchResult {
            let faucet = ensure_signed(origin)?;

            ensure!(amount > Zero::zero(), Error::<T>::ZeroAmount);

            let settings = Self::require_faucet_settings(&faucet)?;
            ensure!(amount <= settings.drop_limit, Error::<T>::DropAmountLimit);

            let maybe_drop = Self::drop_id_by_recipient(&recipient).and_then(Self::drop_by_id);

            let mut is_new_drop = false;
            let mut drop = maybe_drop.unwrap_or_else(|| {
                is_new_drop = true;
                let drop_id = Self::next_drop_id();
                Drop::<T>::new(drop_id)
            });

            if !is_new_drop {
                let current_block = <system::Module<T>>::block_number();
                let last_period_update = current_block.saturating_sub(settings.period.unwrap_or_else(Zero::zero));

                if last_period_update >= drop.last_drop_at {
                    drop.last_drop_at = current_block;
                    if settings.period.is_some() {
                        drop.total_dropped = Zero::zero();
                    }
                }
            }

            let amount_allowed = settings.period_limit.saturating_sub(drop.total_dropped);
            ensure!(amount_allowed >= amount, Error::<T>::FaucetLimitReached);

            T::Currency::transfer(
                &faucet,
                &recipient,
                amount,
                ExistenceRequirement::KeepAlive
            )?;

            drop.total_dropped = drop.total_dropped.saturating_add(amount);

            TotalFaucetDropsByAccount::<T>::mutate(&recipient, &faucet, |total| *total = total.saturating_add(amount));
            DropIdByRecipient::<T>::insert(&recipient, drop.id);
            DropById::<T>::insert(drop.id, drop);
            if is_new_drop {
                NextDropId::mutate(|x| *x += 1);
            }

            Self::deposit_event(RawEvent::Dropped(faucet, recipient, amount));
            Ok(())
        }
    }
}

impl<T: Trait> Module<T> {
    pub fn require_faucet_settings(
        faucet: &T::AccountId
    ) -> Result<FaucetSettings<T::BlockNumber, BalanceOf<T>>, DispatchError> {
        Ok(Self::settings_by_faucet(faucet).ok_or(Error::<T>::FaucetNotFound)?)
    }
}

impl<T: Trait> Drop<T> {
    pub fn new(id: DropId) -> Self {
        Self {
            id,
            last_drop_at: <system::Module<T>>::block_number(),
            total_dropped: Zero::zero(),
        }
    }
}