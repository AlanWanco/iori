pub mod inspectors;

pub use shiori_plugin::*;
use std::{collections::HashMap, time::Duration};
use tokio::time::sleep;

use crate::commands::STYLES;

#[derive(Default)]
pub struct PluginManager {
    /// Whether to wait on found
    wait: Option<u64>,

    plugins: Vec<Box<dyn ShioriPlugin + Send + Sync + 'static>>,
    plugin_inspectors: HashMap<String, Vec<InspectorItem>>,

    temp_inspectors: Vec<InspectorItem>,
}

impl PluginManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add inspector to front queue
    pub fn add(&mut self, plugin: impl ShioriPlugin + Send + Sync + 'static) -> &mut Self {
        plugin.register(self).unwrap();
        let mut inspectors = std::mem::take(&mut self.temp_inspectors);
        self.plugin_inspectors
            .entry(plugin.name().to_string())
            .and_modify(|e| e.extend(std::mem::take(&mut inspectors)))
            .or_insert(inspectors);
        self.plugins.push(Box::new(plugin));

        self
    }

    pub fn wait(mut self, value: bool) -> Self {
        self.wait = if value { Some(5) } else { None };
        self
    }

    pub fn wait_for(mut self, value: u64) -> Self {
        self.wait = Some(value);
        self
    }

    pub fn help(self) -> String {
        let mut is_first = true;

        let mut result = format!("{style}Plugins:{style:#}\n", style = STYLES.get_header());

        for plugin in self.plugins.iter() {
            if !is_first {
                result.push('\n');
            }
            is_first = false;

            result.push_str(&format!(
                "  {style}{}:{style:#}\n",
                plugin.name(),
                style = STYLES.get_literal()
            ));
            for line in plugin
                .description()
                .unwrap_or_else(|| "<No description>".into())
                .split('\n')
            {
                result.push_str(&" ".repeat(10));
                result.push_str(line);
                result.push('\n');
            }
            if let Some(long) = plugin.description_long() {
                for line in long.split('\n') {
                    result.push_str(&" ".repeat(10));
                    result.push_str(line);
                    result.push('\n');
                }
            }
            if let Some(inspectors) = self.plugin_inspectors.get(plugin.name().as_ref()) {
                result.push('\n');

                for inspector in inspectors {
                    result.push_str(&" ".repeat(10));
                    result.push_str(&format!(
                        "{style}{}: {style:#}\n",
                        inspector.inspector.name(),
                        style = STYLES.get_valid()
                    ));
                    result.push_str(&" ".repeat(14));
                    result.push_str(inspector.regex.as_str());

                    result.push('\n');
                }
            }
        }
        result
    }

    pub fn add_arguments(&self, command: &mut impl InspectorCommand) {
        for plugin in self.plugins.iter() {
            plugin.arguments(command);
        }
    }

    pub async fn inspect(
        self,
        context: &ShioriContext,
        url: &str,
        args: &dyn InspectorArguments,
        choose_candidate: fn(Vec<InspectCandidate>) -> InspectCandidate,
    ) -> anyhow::Result<(Cow<'static, str>, Vec<InspectPlaylist>)> {
        let mut url = Cow::Borrowed(url);

        let mut inspectors: Vec<_> = self.plugin_inspectors.values().flatten().collect();
        inspectors.sort_by_key(|item| item.priority);
        inspectors.reverse();

        // As `InspectBranch::Redirect` exists, we need a loop
        let mut retry_count = 0u64;
        let result = 'outer: loop {
            let mut should_retry = false;
            for item in inspectors.iter() {
                // If a regex matches, we try to inspect it
                if let Some(captures) = item.regex.captures(&url) {
                    let inspect_result = item
                        .inspector
                        .inspect(context, &url, &captures, args)
                        .await
                        .inspect_err(|e| log::error!("Failed to inspect {url}: {:?}", e))
                        .ok();
                    let inspect_branch = handle_inspect_result(
                        context,
                        item.inspector.as_ref(),
                        inspect_result,
                        choose_candidate,
                    )
                    .await;
                    match inspect_branch {
                        InspectBranch::Skip => continue,
                        InspectBranch::Redirect(redirect_url) => {
                            url = Cow::Owned(redirect_url);
                            continue 'outer;
                        }
                        InspectBranch::Found(data) => {
                            break 'outer (item.inspector.name(), data);
                        }
                        InspectBranch::NotFound => {
                            if self.wait.is_some() {
                                should_retry = true;
                            } else {
                                anyhow::bail!("Not found")
                            }
                        }
                    }
                }
            }

            if should_retry {
                let wait_time = self.wait.expect("retry requires a wait interval");
                retry_count += 1;
                log::info!(
                    "No playable source found; retrying inspection in {wait_time} seconds (attempt {retry_count})."
                );
                sleep(Duration::from_secs(wait_time)).await;
                continue 'outer;
            }

            anyhow::bail!("No inspector matched")
        };

        Ok(result)
    }
}

impl InspectorRegistry for PluginManager {
    fn register_inspector(
        &mut self,
        regex: Regex,
        inspector: Box<dyn Inspect>,
        priority_hint: PriorityHint,
    ) {
        self.temp_inspectors.push(InspectorItem {
            regex,
            inspector,
            priority: priority_hint,
        });
    }
}

struct InspectorItem {
    regex: Regex,
    inspector: Box<dyn Inspect + Send + Sync + 'static>,
    priority: PriorityHint,
}

enum InspectBranch {
    Skip,
    Redirect(String),
    Found(Vec<InspectPlaylist>),
    NotFound,
}

#[async_recursion::async_recursion]
async fn handle_inspect_result(
    context: &ShioriContext,
    inspector: &dyn Inspect,
    result: Option<InspectResult>,
    choose_candidate: fn(Vec<InspectCandidate>) -> InspectCandidate,
) -> InspectBranch {
    match result {
        Some(InspectResult::NotMatch) => InspectBranch::Skip,
        Some(InspectResult::Candidates(candidates)) => {
            let candidate = choose_candidate(candidates);
            let result = inspector
                .inspect_candidate(context, candidate)
                .await
                .inspect_err(|e| log::error!("Failed to inspect candidate: {:?}", e))
                .ok();
            handle_inspect_result(context, inspector, result, choose_candidate).await
        }
        Some(InspectResult::Playlist(data)) => InspectBranch::Found(vec![data]),
        Some(InspectResult::Playlists(data)) => InspectBranch::Found(data),
        Some(InspectResult::Redirect(redirect_url)) => InspectBranch::Redirect(redirect_url),
        Some(InspectResult::None) | None => InspectBranch::NotFound,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct TestPlugin(Arc<AtomicUsize>);

    impl ShioriPlugin for TestPlugin {
        fn name(&self) -> Cow<'static, str> {
            "wait-test".into()
        }

        fn version(&self) -> Cow<'static, str> {
            "0.0.0".into()
        }

        fn description(&self) -> Option<Cow<'static, str>> {
            None
        }

        fn register(&self, registry: &mut dyn InspectorRegistry) -> anyhow::Result<()> {
            registry.register_inspector(
                Regex::new("example\\.test")?,
                Box::new(TestInspector(self.0.clone())),
                PriorityHint::Normal,
            );
            Ok(())
        }
    }

    struct TestInspector(Arc<AtomicUsize>);

    #[async_trait]
    impl Inspect for TestInspector {
        fn name(&self) -> Cow<'static, str> {
            "wait-test-inspector".into()
        }

        async fn inspect(
            &self,
            _context: &ShioriContext,
            _url: &str,
            _captures: &Captures,
            _args: &dyn InspectorArguments,
        ) -> anyhow::Result<InspectResult> {
            if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(InspectResult::None)
            } else {
                Ok(InspectResult::Playlist(InspectPlaylist::default()))
            }
        }
    }

    struct EmptyArguments;

    impl InspectorArguments for EmptyArguments {
        fn get_string(&self, _argument: &'static str) -> Option<String> {
            None
        }

        fn get_boolean(&self, _argument: &'static str) -> bool {
            false
        }
    }

    fn choose_candidate(mut candidates: Vec<InspectCandidate>) -> InspectCandidate {
        candidates.remove(0)
    }

    #[tokio::test]
    async fn wait_retries_inspection_until_a_source_is_available() -> anyhow::Result<()> {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut manager = PluginManager::new();
        manager.add(TestPlugin(calls.clone()));
        let manager = manager.wait_for(0);
        let context = ShioriContext::new(iori::IoriHttp::new(reqwest::Client::builder));

        manager
            .inspect(
                &context,
                "https://example.test/video/123",
                &EmptyArguments,
                choose_candidate,
            )
            .await?;

        assert_eq!(calls.load(Ordering::SeqCst), 2);
        Ok(())
    }
}
