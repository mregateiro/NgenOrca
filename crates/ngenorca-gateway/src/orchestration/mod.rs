//! Multi-agent orchestration implementation.
//!
//! Contains the concrete implementations of:
//! - [`RuleBasedClassifier`] — regex/keyword-based task classification (zero cost)
//! - [`HeuristicQualityGate`] — length/format quality checks (zero cost)
//! - [`HybridOrchestrator`] — the main orchestrator that cascades:
//!   rules → SLM classifier → LLM, with quality gate and learning

pub mod classifier;
pub mod learned;
pub mod quality;
pub mod orchestrator;

pub use classifier::RuleBasedClassifier;
pub use learned::LearnedRouter;
pub use quality::HeuristicQualityGate;
pub use orchestrator::HybridOrchestrator;
