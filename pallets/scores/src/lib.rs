#![cfg_attr(not(feature = "std"), no_std)]
#![allow(clippy::string_lit_as_bytes)]

use codec::{Decode, Encode};
use frame_support::{
    decl_error, decl_event, decl_module, decl_storage,
    dispatch::DispatchResult, ensure, traits::Get,
};
use sp_runtime::RuntimeDebug;
use sp_std::prelude::*;

use pallet_posts::{Module as Posts, Post, PostById, PostExtension, PostId};
use pallet_profiles::{Module as Profiles, SocialAccountById};
use pallet_reactions::ReactionKind;
use pallet_spaces::{Module as Spaces, SpaceById};
use pallet_utils::log_2;

// mod tests;

#[derive(Encode, Decode, Clone, Copy, Eq, PartialEq, RuntimeDebug)]
pub enum ScoringAction {
    UpvotePost,
    DownvotePost,
    SharePost,
    CreateComment,
    UpvoteComment,
    DownvoteComment,
    ShareComment,
    FollowSpace,
    FollowAccount,
}

impl Default for ScoringAction {
    fn default() -> Self {
        ScoringAction::FollowAccount
    }
}

/// The pallet's configuration trait.
pub trait Trait: system::Trait
    + pallet_utils::Trait
    + pallet_profiles::Trait
    + pallet_posts::Trait
    + pallet_spaces::Trait
{
    /// The overarching event type.
    type Event: From<Event<Self>> + Into<<Self as system::Trait>::Event>;

    // Weights of the social actions
    type FollowSpaceActionWeight: Get<i16>;
    type FollowAccountActionWeight: Get<i16>;

    type SharePostActionWeight: Get<i16>;
    type UpvotePostActionWeight: Get<i16>;
    type DownvotePostActionWeight: Get<i16>;

    type CreateCommentActionWeight: Get<i16>;
    type ShareCommentActionWeight: Get<i16>;
    type UpvoteCommentActionWeight: Get<i16>;
    type DownvoteCommentActionWeight: Get<i16>;
}

decl_error! {
    pub enum Error for Module<T: Trait> {
        /// Scored account reputation difference by account and action not found.
        ReputationDiffNotFound,
        /// Post extension is a comment.
        PostIsAComment,
        /// Post extension is not a comment.
        PostIsNotAComment,

        /// Out of bounds increasing a space score.
        SpaceScoreOverflow,
        /// Out of bounds decreasing a space score.
        SpaceScoreUnderflow,
        /// Out of bounds increasing a post score.
        PostScoreOverflow,
        /// Out of bounds decreasing a post score.
        PostScoreUnderflow,
        /// Out of bounds increasing a comment score.
        CommentScoreOverflow,
        /// Out of bounds decreasing a comment score.
        CommentScoreUnderflow,
        /// Out of bounds increasing a reputation of a social account.
        ReputationOverflow,
        /// Out of bounds decreasing a reputation of a social account.
        ReputationUnderflow,
    }
}

// This pallet's storage items.
decl_storage! {
    trait Store for Module<T: Trait> as TemplateModule {
        pub AccountReputationDiffByAccount get(fn account_reputation_diff_by_account): map (T::AccountId, T::AccountId, ScoringAction) => Option<i16>; // TODO shorten name (?refactor)
        pub PostScoreByAccount get(fn post_score_by_account): map (T::AccountId, PostId, ScoringAction) => Option<i16>;
    }
}

decl_event!(
    pub enum Event<T> where
        <T as system::Trait>::AccountId,
    {
        AccountReputationChanged(AccountId, ScoringAction, u32),
    }
);

// The pallet's dispatchable functions.
decl_module! {
    pub struct Module<T: Trait> for enum Call where origin: T::Origin {

        /// Weights of the related social account actions
        const FollowSpaceActionWeight: i16 = T::FollowSpaceActionWeight::get();
        const FollowAccountActionWeight: i16 = T::FollowAccountActionWeight::get();
        const UpvotePostActionWeight: i16 = T::UpvotePostActionWeight::get();
        const DownvotePostActionWeight: i16 = T::DownvotePostActionWeight::get();
        const SharePostActionWeight: i16 = T::SharePostActionWeight::get();
        const CreateCommentActionWeight: i16 = T::CreateCommentActionWeight::get();
        const UpvoteCommentActionWeight: i16 = T::UpvoteCommentActionWeight::get();
        const DownvoteCommentActionWeight: i16 = T::DownvoteCommentActionWeight::get();
        const ShareCommentActionWeight: i16 = T::ShareCommentActionWeight::get();

        // Initializing events
        fn deposit_event() = default;
    }
}

impl<T: Trait> Module<T> {
    pub fn scoring_action_by_post_extension(
        extension: PostExtension,
        reaction_kind: ReactionKind,
        reverse: bool,
    ) -> ScoringAction {
        match extension {
            PostExtension::RegularPost | PostExtension::SharedPost(_) => match reaction_kind {
                ReactionKind::Upvote =>
                    if reverse { ScoringAction::DownvotePost } else { ScoringAction::UpvotePost },
                ReactionKind::Downvote =>
                    if reverse { ScoringAction::UpvotePost } else { ScoringAction::DownvotePost },
            },
            PostExtension::Comment(_) => match reaction_kind {
                ReactionKind::Upvote =>
                    if reverse { ScoringAction::DownvoteComment } else { ScoringAction::UpvoteComment },
                ReactionKind::Downvote =>
                    if reverse { ScoringAction::UpvoteComment } else { ScoringAction::DownvoteComment },
            },
        }
    }

    pub fn change_post_score_by_extension(
        account: T::AccountId,
        post: &mut Post<T>,
        action: ScoringAction,
    ) -> DispatchResult {
        if post.is_comment() {
            Self::change_comment_score(account, post, action)?;
        } else {
            Self::change_post_score(account, post, action)?;
        }

        Ok(())
    }

    fn change_post_score(
        account: T::AccountId,
        post: &mut Post<T>,
        action: ScoringAction,
    ) -> DispatchResult {
        ensure!(!post.is_comment(), Error::<T>::PostIsAComment);

        let social_account = Profiles::get_or_new_social_account(account.clone());

        // TODO inspect: this insert could be redundant if the account already exists.
        <SocialAccountById<T>>::insert(account.clone(), social_account.clone());

        let post_id = post.id;

        // TODO inspect: maybe this check is redundant such as we use change_post_score() internally and post was already loaded.
        Posts::<T>::ensure_post_exists(post_id)?;

        if let Some(post_space_id) = post.space_id {
            let mut space = Spaces::require_space(post_space_id)?;

            // TODO replace with !post.is_owner(account)
            if post.created.account != account {
                if let Some(score_diff) = Self::post_score_by_account((account.clone(), post_id, action)) {
                    let reputation_diff = Self::account_reputation_diff_by_account((account.clone(), post.created.account.clone(), action))
                        .ok_or(Error::<T>::ReputationDiffNotFound)?;

                    post.score = post.score.checked_sub(score_diff as i32).ok_or(Error::<T>::PostScoreUnderflow)?;
                    space.score = space.score.checked_sub(score_diff as i32).ok_or(Error::<T>::SpaceScoreUnderflow)?;
                    Self::change_social_account_reputation(post.created.account.clone(), account.clone(), -reputation_diff, action)?;
                    <PostScoreByAccount<T>>::remove((account, post_id, action));
                } else {
                    match action {
                        ScoringAction::UpvotePost => {
                            if Self::post_score_by_account((account.clone(), post_id, ScoringAction::DownvotePost)).is_some() {
                                Self::change_post_score(account.clone(), post, ScoringAction::DownvotePost)?;
                            }
                        }
                        ScoringAction::DownvotePost => {
                            if Self::post_score_by_account((account.clone(), post_id, ScoringAction::UpvotePost)).is_some() {
                                Self::change_post_score(account.clone(), post, ScoringAction::UpvotePost)?;
                            }
                        }
                        _ => (),
                    }
                    let score_diff = Self::score_diff_for_action(social_account.reputation, action);
                    post.score = post.score.checked_add(score_diff as i32).ok_or(Error::<T>::PostScoreOverflow)?;
                    space.score = space.score.checked_add(score_diff as i32).ok_or(Error::<T>::SpaceScoreOverflow)?;
                    Self::change_social_account_reputation(post.created.account.clone(), account.clone(), score_diff, action)?;
                    <PostScoreByAccount<T>>::insert((account, post_id, action), score_diff);
                }

                <PostById<T>>::insert(post_id, post.clone());
                <SpaceById<T>>::insert(post_space_id, space);
            }
        }

        Ok(())
    }

    fn change_comment_score(
        account: T::AccountId,
        comment: &mut Post<T>,
        action: ScoringAction,
    ) -> DispatchResult {
        ensure!(comment.is_comment(), Error::<T>::PostIsNotAComment);

        let social_account = Profiles::get_or_new_social_account(account.clone());

        // TODO inspect: this insert could be redundant if the account already exists.
        <SocialAccountById<T>>::insert(account.clone(), social_account.clone());

        let comment_id = comment.id;

        // TODO inspect: maybe this check is redundant such as we use change_comment_score() internally and comment was already loaded.
        Posts::<T>::ensure_post_exists(comment_id)?;

        // TODO replace with !comment.is_owner(account)
        if comment.created.account != account {
            if let Some(score_diff) = Self::post_score_by_account((account.clone(), comment_id, action)) {
                let reputation_diff = Self::account_reputation_diff_by_account((account.clone(), comment.created.account.clone(), action))
                    .ok_or(Error::<T>::ReputationDiffNotFound)?;

                comment.score = comment.score.checked_sub(score_diff as i32).ok_or(Error::<T>::CommentScoreUnderflow)?;
                Self::change_social_account_reputation(comment.created.account.clone(), account.clone(), -reputation_diff, action)?;
                <PostScoreByAccount<T>>::remove((account, comment_id, action));
            } else {
                match action {
                    ScoringAction::UpvoteComment => {
                        if Self::post_score_by_account((account.clone(), comment_id, ScoringAction::DownvoteComment)).is_some() {
                            Self::change_comment_score(account.clone(), comment, ScoringAction::DownvoteComment)?;
                        }
                    }
                    ScoringAction::DownvoteComment => {
                        if Self::post_score_by_account((account.clone(), comment_id, ScoringAction::UpvoteComment)).is_some() {
                            Self::change_comment_score(account.clone(), comment, ScoringAction::UpvoteComment)?;
                        }
                    }
                    ScoringAction::CreateComment => {
                        let root_post = &mut comment.get_root_post()?;
                        Self::change_post_score(account.clone(), root_post, action)?;
                    }
                    _ => (),
                }
                let score_diff = Self::score_diff_for_action(social_account.reputation, action);
                comment.score = comment.score.checked_add(score_diff as i32).ok_or(Error::<T>::CommentScoreOverflow)?;
                Self::change_social_account_reputation(comment.created.account.clone(), account.clone(), score_diff, action)?;
                <PostScoreByAccount<T>>::insert((account, comment_id, action), score_diff);
            }
            <PostById<T>>::insert(comment_id, comment.clone());
        }

        Ok(())
    }

    pub fn change_social_account_reputation(
        account: T::AccountId,
        scorer: T::AccountId,
        mut score_diff: i16,
        action: ScoringAction,
    ) -> DispatchResult {

        // TODO return Ok(()) if score_diff == 0?

        let mut social_account = Profiles::get_or_new_social_account(account.clone());

        if social_account.reputation as i64 + score_diff as i64 <= 1 {
            social_account.reputation = 1;
            score_diff = 0;
        }

        if score_diff > 0 {
            social_account.reputation = social_account.reputation
                .checked_add(score_diff as u32)
                .ok_or(Error::<T>::ReputationOverflow)?;
        } else if score_diff < 0 {
            social_account.reputation = social_account.reputation
                .checked_sub(score_diff as u32)
                .ok_or(Error::<T>::ReputationUnderflow)?;
        }

        if Self::account_reputation_diff_by_account((scorer.clone(), account.clone(), action)).is_some() {
            <AccountReputationDiffByAccount<T>>::remove((scorer, account.clone(), action));
        } else {
            <AccountReputationDiffByAccount<T>>::insert((scorer, account.clone(), action), score_diff);
        }

        <SocialAccountById<T>>::insert(account.clone(), social_account.clone());

        Self::deposit_event(RawEvent::AccountReputationChanged(account, action, social_account.reputation));

        Ok(())
    }

    pub fn score_diff_for_action(reputation: u32, action: ScoringAction) -> i16 {
        Self::smooth_reputation(reputation) as i16 * Self::weight_of_scoring_action(action)
    }

    fn smooth_reputation(reputation: u32) -> u8 {
        log_2(reputation).map_or(1, |r| {
            let d = (reputation as u64 - (2 as u64).pow(r)) * 100
                / (2 as u64).pow(r);

            // We can safely cast this result to i16 because a score diff for u32::MAX is 32.
            (((r + 1) * 100 + d as u32) / 100) as u8
        })
    }

    fn weight_of_scoring_action(action: ScoringAction) -> i16 {
        use ScoringAction::*;
        match action {
            UpvotePost => T::UpvotePostActionWeight::get(),
            DownvotePost => T::DownvotePostActionWeight::get(),
            SharePost => T::SharePostActionWeight::get(),
            CreateComment => T::CreateCommentActionWeight::get(),
            UpvoteComment => T::UpvoteCommentActionWeight::get(),
            DownvoteComment => T::DownvoteCommentActionWeight::get(),
            ShareComment => T::ShareCommentActionWeight::get(),
            FollowSpace => T::FollowSpaceActionWeight::get(),
            FollowAccount => T::FollowAccountActionWeight::get(),
        }
    }
}