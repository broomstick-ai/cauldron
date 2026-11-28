//! The service's cauldron seam: the line protocol on `$CAULDRON_BIND`,
//! registered through shapewire's seam with the warnings attestation, as
//! every cauldron service does. Its own verbs:
//!
//! - `MCP` answers where the door is (`http://127.0.0.1:<port>/mcp`): how a
//!   client in an environment other than `prod` finds a door whose port the
//!   kernel chose;
//! - `OPEN` answers, as one JSON line, the cauldrons this service holds open
//!   for writing (`[{cauldron, opened_at, recovered_at_open}]`), for
//!   `cauldron mcp status`. Names and times only, never soup content;
//! - `DUMPS` answers the same for the dumps stores it holds;
//! - `RDUMP`, `RTEXT`, `RREAD`, `RRUN`, `RPUT`, `RCHECK`, `RDUMPS`, `RNOTE`,
//!   `RFAIL`: the ripple verbs, answered
//!   only to a line carrying the ripples key (`crate::ripple`);
//! - `DECIDE`: the owner's decision on a ripple's records (approve,
//!   unapprove, reject, unreject, or approve some and reject others at
//!   once), answered only to a line carrying the approval key, which only
//!   the owner's commands at a terminal read (`crate::approve`);
//! - `ADECIDE`: the same decision, sent by the owner's command line without
//!   a terminal where the owner's `owner_acts` choice opens `approve`, and
//!   recorded as an agent's (`by: "agent"`); a verb of its own so that an
//!   older service refuses it instead of recording it as a person's;
//! - `MERGE`: a merge into a cauldron this service holds, from `cauldron
//!   merge` at a terminal, with the same approval key (`crate::merge`);
//! - `AMERGE`: the same, from `cauldron merge` without a terminal where the
//!   owner's persisted owner_acts setting opens `merge`, recorded as an
//!   agent's (`by: "agent"`); a verb of its own for the same reason as
//!   `ADECIDE`;
//! - `APPROVE`: the earlier command line's approval by id (`{cauldron, ids,
//!   unapprove?}` and nothing else), kept for a command line older than
//!   `DECIDE`, with the same key.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{bail, Result};
use env::seams::ServiceEnv;
use env::warnings::{Conn, Export, Service, ServesWarnings, Verb};

/// One running instance, as cauldron's line protocol sees it.
pub struct McpSeam {
    pub se: ServiceEnv,
    pub addr: SocketAddr,
    /// Where `POST /mcp` listens.
    pub door: SocketAddr,
    /// This process's start, the warnings' counting window.
    pub started_at: String,
    /// The door, for `OPEN`.
    pub state: Arc<crate::door::Door>,
    /// The ripples key minted at this start (`crate::ripple`).
    pub ripples: crate::secret::Secret,
    /// The approval key minted at this start (`crate::approve`).
    pub approve: crate::secret::Secret,
}

impl ServesWarnings for McpSeam {
    const SHAPE: &'static str = env::warnings::SHAPE;

    fn warnings(&self, asked: &str) -> Result<Export> {
        if !asked.is_empty() && asked != Self::SHAPE {
            bail!("this service serves the `{}` shape, not `{asked}`", Self::SHAPE);
        }
        Ok(shapewire::warnings::COUNTER.export(&self.se.service, Self::SHAPE, &self.started_at))
    }
}

impl Service for McpSeam {
    const VERBS: &'static [Verb<McpSeam>] = &[
        Verb::new("MCP", McpSeam::v_mcp),
        Verb::new("OPEN", McpSeam::v_open),
        Verb::new("DUMPS", McpSeam::v_dumps),
        Verb::new("PING", McpSeam::v_ping),
        Verb::new("RDUMP", McpSeam::v_rdump),
        Verb::new("RTEXT", McpSeam::v_rtext),
        Verb::new("RREAD", McpSeam::v_rread),
        Verb::new("RRUN", McpSeam::v_rrun),
        Verb::new("RPUT", McpSeam::v_rput),
        Verb::new("RCHECK", McpSeam::v_rcheck),
        Verb::new("APPROVE", McpSeam::v_approve),
        Verb::new("DECIDE", McpSeam::v_decide),
        Verb::new("MERGE", McpSeam::v_merge),
        Verb::new("AMERGE", McpSeam::v_amerge),
        Verb::new("ADECIDE", McpSeam::v_adecide),
        Verb::new("RDUMPS", McpSeam::v_rdumps),
        Verb::new("RNOTE", McpSeam::v_rnote),
        Verb::new("RFAIL", McpSeam::v_rfail),
        Verb::new("RLOCK", McpSeam::v_rlock),
        Verb::new("RRENEW", McpSeam::v_rrenew),
        Verb::new("RUNLOCK", McpSeam::v_runlock),
    ];

    fn service_env(&self) -> &ServiceEnv {
        &self.se
    }

    fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl McpSeam {
    fn v_mcp(&self, c: &mut Conn<'_>, _rest: &str) -> Result<()> {
        c.line(&format!("http://{}{}", self.door, shapewire::mcp::http::PATH))?;
        Ok(())
    }

    fn v_open(&self, c: &mut Conn<'_>, _rest: &str) -> Result<()> {
        c.line(&serde_json::to_string(&self.state.writers.snapshot())?)?;
        Ok(())
    }

    fn v_dumps(&self, c: &mut Conn<'_>, _rest: &str) -> Result<()> {
        c.line(&serde_json::to_string(&self.state.dumps.snapshot())?)?;
        Ok(())
    }

    fn v_ping(&self, c: &mut Conn<'_>, _rest: &str) -> Result<()> {
        c.line("PONG")?;
        Ok(())
    }

    fn ripple(&self, c: &mut Conn<'_>, verb: &str, rest: &str) -> Result<()> {
        c.line(&crate::ripple::answer(&self.state, &self.ripples, verb, rest))?;
        Ok(())
    }

    fn v_rdump(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        self.ripple(c, "RDUMP", rest)
    }

    fn v_rtext(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        self.ripple(c, "RTEXT", rest)
    }

    fn v_rread(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        self.ripple(c, "RREAD", rest)
    }

    fn v_rrun(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        self.ripple(c, "RRUN", rest)
    }

    fn v_rput(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        self.ripple(c, "RPUT", rest)
    }

    fn v_rcheck(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        self.ripple(c, "RCHECK", rest)
    }

    fn v_rdumps(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        self.ripple(c, "RDUMPS", rest)
    }

    fn v_rnote(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        self.ripple(c, "RNOTE", rest)
    }

    fn v_rfail(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        self.ripple(c, "RFAIL", rest)
    }

    fn v_rlock(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        self.ripple(c, "RLOCK", rest)
    }

    fn v_rrenew(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        self.ripple(c, "RRENEW", rest)
    }

    fn v_runlock(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        self.ripple(c, "RUNLOCK", rest)
    }

    fn v_approve(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        c.line(&crate::approve::answer_approve(&self.state, &self.approve, rest))?;
        Ok(())
    }

    fn v_decide(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        c.line(&crate::approve::answer_decide(&self.state, &self.approve, rest))?;
        Ok(())
    }

    fn v_adecide(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        c.line(&crate::approve::answer_adecide(&self.state, &self.approve, rest))?;
        Ok(())
    }

    fn v_merge(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        c.line(&crate::merge::answer(&self.state, &self.approve, rest))?;
        Ok(())
    }

    fn v_amerge(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        c.line(&crate::merge::answer_agent(&self.state, &self.approve, rest))?;
        Ok(())
    }
}

/// The `tracing` layer that counts every WARN and ERROR by call site.
pub struct WarningsLayer;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for WarningsLayer {
    fn on_event(&self, e: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        shapewire::warnings::COUNTER.record_event(e);
    }
}
