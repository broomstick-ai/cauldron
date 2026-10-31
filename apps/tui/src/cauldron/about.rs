//! **What every Config row means** (8e4fca10 msg 186, V7: "EVERY Config
//! item has one" — audit 0177). A setting's ⓘ says what its setting does
//! (`settings::help`); this is the explanation for every OTHER row a Config
//! tab draws — a band (a submenu or a section), a read-only fact, an action,
//! an item of a list, a quoted thing — so the ⓘ in the
//! rightmost column is on every row, and a click on it (or `h`) always says
//! something true about that row.

use crate::cauldron::app::App;
use crate::cauldron::manage::Act;
use crate::cauldron::menu::{pool_label, Add, Entry, SPage};
use crate::cauldron::settings;

/// What band `page` holds.
pub fn page(p: &SPage) -> String {
    match p {
        SPage::Tab(t) => format!("The {} tab's top level.", t.label()),
        SPage::UseAccount(_, a) => format!("Account {a} as this cauldron takes it: whether it is used here, this cauldron's weekly threshold on it, and whether this cauldron takes advantage of it while it is opportunistic."),
        SPage::Paths => "Every root this cauldron's files sit under. A cluster's Nodes tab, its Directories and Repos, open them. The root of every cauldron and the weights path are on Devices › Storage.".into(),
        SPage::Use => "Which of the global accounts THIS cauldron uses, a table per source: a click on an account opens its settings here. An account not used here stays on for every other cauldron.".into(),
        SPage::Pool(id) => format!("{}: its accounts — a click on one opens its settings — then what every account of it inherits, how it places work, and the models it was detected to offer.", pool_label(id)),
        SPage::Endpoints => "Every inference endpoint every cauldron may spend: one API key at one url each — OpenRouter, the Anthropic API, OpenAI, Gemini, a local server, any other server that speaks the same protocol. A click on one opens its settings; + add endpoint makes one from a template. Below the table, what a new endpoint starts with.".into(),
        SPage::UseEndpoints => "Every inference endpoint this cauldron can see — the global ones and its own — whether it is used here, and the most this cauldron may draw from each key. + add endpoint here registers one only this cauldron sees.".into(),
        SPage::Endpoint { here: true, .. } => "An inference endpoint registered in this cauldron alone: one key at one url, its models and its budget.".into(),
        SPage::Endpoint { .. } => "An inference endpoint every cauldron may spend: one key at one url, the models the key may serve, and its budget.".into(),
        SPage::UseEndpoint(_) => "A global inference endpoint as this cauldron takes it: whether it is used here, its url and models here, and the most this cauldron may draw from the key.".into(),
        SPage::Account(_, a) => format!("Account {a}: its switch, its thresholds and caps, its share, whether it may be opportunistic, rename and remove."),
        SPage::Local => "Local inference: models run on this machine instead of an account — the engine, which models, and how much memory, disk and GPU they may take.".into(),
        SPage::Roles => "Which model each role runs on — a lineage role (curator, builder, …), a ripple's writer, the voice — and what it falls back to.".into(),
        SPage::Role(r) => format!("The {} role: {}. Its model, its fallback, and the token source it spends.", settings::role_name(r), settings::role_of(r).map_or("a role", |x| x.what)),
        SPage::Machine => "This machine: how much memory, CPU, child processes, GPU and disk the cauldron may take, and what it does on battery.".into(),
        SPage::Storage => "Where every cauldron and the model weights live, then what is kept and for how long: threads, logs, observations, backups, voice audio, and garbage collection.".into(),
        SPage::Gc => "Garbage collection: how often stale files are swept, whether it only reports what it would delete, and whether disk pressure triggers a sweep early.".into(),
        SPage::Identity => "How the cauldron knows the operator is the one speaking: the socket secret, the machine's live login, the operator's name.".into(),
        SPage::Attention => "When the cauldron decides for itself and when it interrupts: the interrupt floor, the spoken voice, tones, limit alarms and quiet hours.".into(),
        SPage::Mesh => "The mesh: the supervisor, the services it keeps running, the threads daemon, and the devices work may move between.".into(),
        SPage::Supervisor => "The supervisor: whether a failed service is restarted or rolled back, how fast it is polled, and how restarts back off.".into(),
        SPage::Daemon => "The threads daemon: how often it probes accounts, how it settles, and where it listens.".into(),
        SPage::Devices => "The devices on the mesh, and whether work may be sent to or accepted from them.".into(),
        SPage::Repos => "Every registered repo, + register repo, and the roots a trace looks under.".into(),
        SPage::Repo(n) => format!("Repo {n}: where it is, and unregister."),
        SPage::Region(n) => format!("Region {n} of the soup: its boundary and its nodes, by kind."),
        SPage::Node(n) => format!("Soup node {n}: its line, where it sits, and its body."),
        SPage::List(b) => format!("{}: a list. + add puts an item on it; x on an item takes it off.", b.def().label),
        SPage::Section(p, t) => format!("{t}: one part of {}.", p.title()),
    }
}

/// What read-only fact `label` reports.
pub fn fact(label: &str) -> String {
    let says = match label {
        "warnings cap" => "How large the warnings ledger may grow before its oldest entries go. Fixed in the build, not a setting.",
        "data dir" => "Where the threads daemon keeps its data for this environment.",
        "id" => "This cluster's id: the key its files and tags use. Read only.",
        "repo" | "region" | "node" => "Why this page is empty: what it named is not here any more.",
        "claimed by" => "The clusters that claim this — the ones its work lands on.",
        "narrows" => "The region this one narrows — the wider region it sits inside.",
        "↑" => "Where this node came from (the message it cites) and the regions it sits in.",
        "template" => "What this endpoint was made from: it filled in every row not typed into. Chosen once, when it was added.",
        "server" => "The local server this endpoint calls: its url and whether it needs a key came from it.",
        "headers" => "The extra headers sent on every call to this endpoint. x on one takes it off; + add header puts one on.",
        "enforced by" => "What holds this key to its budget: Cauldron's own meter always, and OpenRouter itself too when the endpoint has OpenRouter's management key.",
        "used by" => "The roles of this cauldron whose work goes to this endpoint.",
        "key" => "The endpoint's key, set on the Providers tab: only how it begins and its last four are ever shown.",
        "budget" => "The key's budget, the wallet every cauldron draws from, set on the Providers tab.",
        _ => "",
    };
    if !says.is_empty() {
        return says.to_string();
    }
    // An account's limit axis (`5h`, `weekly`, …) or a source's reading of one account.
    format!("A reading of {label}: how much of its limit is used and when it resets, as the threads daemon last reported it. Read only.")
}

/// What act `a` does when → starts it.
pub fn act(a: &Act) -> String {
    match a {
        Act::RenameCluster(_) => "Rename this cluster. Its id does not change; only the name he sees.".into(),
        Act::MoveCluster(_) => "Move this cluster, with everything under it, under another cluster.".into(),
        Act::DeleteCluster(_) => "Delete this cluster and every cluster under it. It asks first.".into(),
        Act::RenameAccount { .. } => "Rename this account: the label shown everywhere. Its login is untouched.".into(),
        Act::RemoveAccount { pool, .. } => format!("Take this account out of {}. It asks first; its login directory is kept.", pool_label(pool)),
        Act::NewEndpoint { here: false } => "Add an inference endpoint every cauldron may spend: pick a template, give it its key, and it fills in the rest.".into(),
        Act::NewEndpoint { here: true } => "Add an inference endpoint only this cauldron sees: pick a template, give it its key, and it fills in the rest.".into(),
        Act::AddEndpoint { .. } => "Add the endpoint as it is filled in. It waits for what its template needs: a name, a url, and — for a provider's API — its key.".into(),
        Act::RemoveEndpoint { .. } => "Remove this endpoint and every setting it has. It asks first.".into(),
        Act::OpenEndpoint(_) => "Open this endpoint's own settings, on the Providers tab: its key, its budget, its models.".into(),
        Act::RegisterRepo => "Register a repo by its path, so a cluster can claim it.".into(),
        Act::UnregisterRepo(_) => "Take this repo off the registry. A repo a cluster claims must be let go first.".into(),
        Act::Enable(_) => "Enable or disable this cauldron. It asks first.".into(),
        Act::Service { op, .. } => format!("{} this service. It asks first, and it is recorded in the supervision log below as requested by you.", match op { crate::cauldron::supervise::Op::Restart => "Restart", crate::cauldron::supervise::Op::Stop => "Stop", crate::cauldron::supervise::Op::Rollback => "Roll back" }),
        Act::Gc => "Collect now: shows what a pass would reclaim from this face's own files and asks first.".into(),
    }
}

impl App {
    /// The ⓘ of Config entry `e` (and `h` on it): its title and what it means.
    /// `None` for what is not a row (a gap, a paragraph, the strip).
    pub fn entry_help(&self, e: &Entry) -> Option<(String, String)> {
        Some(match e {
            Entry::Set(bd) | Entry::Run(bd) => (self.row_name(bd), settings::help(bd.def())),
            Entry::Sub { page: p, label, .. } => (label.clone(), page(p)),
            Entry::Row(p) => (self.row_label(p), format!("{} A click opens its settings.", page(p))),
            Entry::Head(t, _) => (t.clone(), format!("{t}: one part of this page.")),
            Entry::Fixed(t, _) => (t.clone(), format!("{t}: always open; each one is switched on or off on its own row.")),
            Entry::Fact(k, _) => (k.clone(), fact(k)),
            Entry::Item { list, text, .. } => (text.clone(), format!("An item of {}. x asks, and x again takes it off the list.", list.def().label)),
            Entry::Add(Add::Account(p)) => ("+ add account".into(), format!("Add a subscription account to {}: it is named, then logged in through the threads daemon.", pool_label(p))),
            Entry::Add(Add::Item(list)) => (crate::cauldron::menu::add_words(list), format!("Put an item on {}.", list.def().label)),
            Entry::Do { act: a, label, .. } => (label.trim_start_matches("+ ").to_string(), act(a)),
            Entry::Ref { tag, label, .. } => (label.clone(), format!("{label}. `i` quotes it into the bucket as {}.", tag.render())),
            Entry::Strip | Entry::Gap | Entry::Para(_) => return None,
        })
    }
}
