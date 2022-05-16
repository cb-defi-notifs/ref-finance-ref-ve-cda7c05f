use crate::*;

#[derive(BorshSerialize, BorshDeserialize, Serialize)]
#[cfg_attr(not(target_arch = "wasm32"), derive(Debug, Deserialize))]
#[serde(crate = "near_sdk::serde")]
pub struct Account {
    /// The amount of LPT locked
    #[serde(with = "u128_dec_format")]
    pub lpt_amount: Balance,
    /// The amount of veLPT the account holds
    #[serde(with = "u128_dec_format")]
    pub ve_lpt_amount: Balance,
    /// When the locking token can be unlocked without slash in nanoseconds.
    #[serde(with = "u64_dec_format")]
    pub unlock_timestamp: u64,
    /// The duration of current locking in seconds.
    pub duration_sec: u32,
    /// Record voting action
    pub proposals: HashMap<u32, Action>,
    #[serde(with = "u128_map_format")]
    pub rewards: HashMap<AccountId, Balance>,
}

impl Default for Account {
    fn default() -> Self {
        Account {
            lpt_amount: 0,
            ve_lpt_amount: 0,
            unlock_timestamp: 0,
            duration_sec: 0,
            proposals: HashMap::new(),
            rewards: HashMap::new()
        }
    }
}


#[derive(BorshSerialize, BorshDeserialize)]
pub enum VAccount {
    Current(Account),
}

impl From<VAccount> for Account {
    fn from(v: VAccount) -> Self {
        match v {
            VAccount::Current(c) => c,
        }
    }
}

impl From<Account> for VAccount {
    fn from(c: Account) -> Self {
        VAccount::Current(c)
    }
}

impl Account {
    pub fn new() -> Self {
        Account {
            lpt_amount: 0,
            ve_lpt_amount: 0,
            unlock_timestamp: 0,
            duration_sec: 0,
            proposals: HashMap::new(),
            rewards: HashMap::new()
        }
    }

    pub fn add_rewards(&mut self, rewards: &HashMap<AccountId, Balance>) {
        for (reward_token, reward) in rewards {
            self.rewards.insert(
                reward_token.clone(),
                (reward + self.rewards.get(reward_token).unwrap_or(&0_u128)).clone(),
            );
        }
    }

    pub fn sub_reward(&mut self, token_id: &AccountId, amount: Balance) {
        if let Some(prev) = self.rewards.remove(token_id) {
            require!(amount <= prev, E101_INSUFFICIENT_BALANCE);
            let remain = prev - amount;
            if remain > 0 {
                self.rewards.insert(token_id.clone(), remain);
            }
        }
    }

    pub fn lock_lpt(&mut self, amount: Balance, duration_sec: u32, config: &Config) -> Balance {
        let prev = self.ve_lpt_amount;

        let timestamp = env::block_timestamp();
        let new_unlock_timestamp = timestamp + to_nano(duration_sec);

        if self.unlock_timestamp > 0 && self.unlock_timestamp > timestamp {
            // exist lpt locked need relock
            require!(self.unlock_timestamp <= new_unlock_timestamp, E304_CAUSE_PRE_UNLOCK);
            let relocked_ve = compute_ve_lpt_amount(&config, self.lpt_amount, duration_sec);
            self.ve_lpt_amount = std::cmp::max(self.ve_lpt_amount, relocked_ve);
            let extra_x = compute_ve_lpt_amount(config, amount, duration_sec);
            self.ve_lpt_amount += extra_x;
        } else {
            self.ve_lpt_amount = compute_ve_lpt_amount(config, self.lpt_amount + amount, duration_sec);
        }
        self.unlock_timestamp = new_unlock_timestamp;
        self.lpt_amount += amount;
        self.duration_sec = duration_sec;

        self.ve_lpt_amount - prev
    }

    pub fn withdraw_lpt(&mut self, amount: u128) -> Balance {
        let prev = self.ve_lpt_amount;

        let timestamp = env::block_timestamp();
        require!(timestamp >= self.unlock_timestamp, E305_STILL_IN_LOCK);
        require!(amount <= self.lpt_amount && amount != 0, E101_INSUFFICIENT_BALANCE);

        if amount < self.lpt_amount {
            let new_ve = u128_ratio(self.ve_lpt_amount, self.lpt_amount - amount, self.lpt_amount);
            self.ve_lpt_amount = new_ve;
        } else {
            self.ve_lpt_amount = 0;
            self.unlock_timestamp = 0;
            self.duration_sec = 0;
        }
        self.lpt_amount -= amount;

        prev - self.ve_lpt_amount
    }
}

impl Contract {
    pub fn update_impacted_proposals(&mut self, account: &mut Account, prev_ve_lpt_amount: Balance, diff_ve_lpt_amount: Balance, is_increased: bool){
        let mut rewards = HashMap::new();
        account.proposals.retain(|proposal_id, action| {
            let mut proposal = self.internal_unwrap_proposal(*proposal_id);
            if proposal.status == Some(ProposalStatus::Expired) {
                self.internal_redeem_near(&mut proposal);
                if let Some((token_id, reward_amount)) = proposal.claim_reward(prev_ve_lpt_amount){
                    rewards.insert(token_id.clone(), reward_amount + rewards.get(&token_id).unwrap_or(&0_u128));
                }
                self.data_mut().proposals.insert(&proposal_id, &proposal.into());
                false
            } else {
                if diff_ve_lpt_amount > 0 {
                    proposal.update_votes(action, diff_ve_lpt_amount, self.data().cur_total_ve_lpt, is_increased);
                    if !is_increased && prev_ve_lpt_amount == diff_ve_lpt_amount {
                        proposal.participants -= 1;
                    }
                    self.data_mut().proposals.insert(&proposal_id, &proposal.into());
                }
                true
            }
        });
        account.add_rewards(&rewards);
    }

    pub fn internal_account_vote(
        &mut self,
        proposer: &AccountId,
        proposal_id: u32,
        action: &Action,
    ) -> Balance {
        let mut account = self.internal_unwrap_account(proposer);
        let ve_lpt_amount = account.ve_lpt_amount;
        require!(ve_lpt_amount > 0, E303_INSUFFICIENT_VE_LPT);
        require!(!account.proposals.contains_key(&proposal_id), E200_ALREADY_VOTED);
        account.proposals.insert(proposal_id, action.clone());
        self.internal_claim_all(&mut account);
        self.data_mut().accounts.insert(proposer, &account.into());
        ve_lpt_amount
    }

    pub fn internal_account_cancel_vote(
        &mut self,
        proposer: &AccountId,
        proposal_id: u32,
    ) -> (Action, Balance) {
        let mut account = self.internal_unwrap_account(proposer);
        let ve_lpt_amount = account.ve_lpt_amount;
        require!(account.proposals.contains_key(&proposal_id), E206_NO_VOTED);
        let action = account.proposals.remove(&proposal_id).unwrap();
        self.internal_claim_all(&mut account);
        self.data_mut().accounts.insert(proposer, &account.into());
        (action, ve_lpt_amount)
    }
}

impl Contract {
    pub fn internal_get_account(&self, account_id: &AccountId) -> Option<Account> {
        self.data().accounts.get(account_id).map(|o| o.into())
    }

    pub fn internal_unwrap_account(&self, account_id: &AccountId) -> Account {
        self.internal_get_account(account_id)
            .expect(E100_ACC_NOT_REGISTERED)
    }

    pub fn internal_set_account(&mut self, account_id: &AccountId, account: Account) {
        self.data_mut().accounts.insert(account_id, &account.into());
    }

    pub fn internal_unwrap_or_default_account(&mut self, account_id: &AccountId) -> Account {
        if let Some(account) = self.internal_get_account(account_id) {
            account
        } else {
            self.data_mut().account_count += 1;
            Account::default()
        }
    }

    pub fn internal_remove_account(&mut self, account_id: &AccountId) {
        self.data_mut().accounts.remove(account_id);
        self.data_mut().account_count -= 1;
        self.ft.accounts.remove(account_id);
    }
}

fn compute_ve_lpt_amount(config: &Config, amount: u128, duration_sec: u32) -> u128 {
    amount
        + u128_ratio(
            amount,
            u128::from(config.max_locking_multiplier - MIN_LOCKING_REWARD_RATIO) * u128::from(to_nano(duration_sec)),
            u128::from(to_nano(config.max_locking_duration_sec)) * MIN_LOCKING_REWARD_RATIO as u128,
        )
}