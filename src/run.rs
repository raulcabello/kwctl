use anyhow::{anyhow, Result};
use policy_evaluator::policy_evaluator::PolicyEvaluator;
use policy_fetcher::{registry::config::DockerConfig, sources::Sources};

use crate::pull;

pub(crate) async fn pull_and_run(
    uri: &str,
    docker_config: Option<DockerConfig>,
    sources: Option<Sources>,
    request: &str,
    settings: Option<String>,
) -> Result<()> {
    let policy_path = pull::pull(
        uri,
        docker_config,
        sources,
        policy_fetcher::PullDestination::MainStore,
    )
    .await
    .map_err(|e| anyhow!("Error pulling policy {}: {}", uri, e))?;

    let request = serde_json::from_str::<serde_json::Value>(&request)?;

    println!(
        "{}",
        serde_json::to_string(
            &PolicyEvaluator::new(
                policy_path.as_path(),
                settings.map_or(Ok(None), |settings| serde_yaml::from_str(&settings))?,
            )?
            .validate(
                {
                    match request {
                        serde_json::Value::Object(ref object) => {
                            if object.get("kind").and_then(serde_json::Value::as_str)
                                == Some("AdmissionReview")
                            {
                                object
                                    .get("request")
                                    .ok_or_else(|| anyhow!("invalid admission review object"))
                            } else {
                                Ok(&request)
                            }
                        }
                        _ => Err(anyhow!("request to evaluate is invalid")),
                    }
                }?
                .clone()
            )
        )?
    );
    Ok(())
}
