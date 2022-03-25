use crate::{DockerConfig, Registry, Sources};
use anyhow::{anyhow, Result};
use mdcat::{ResourceAccess, TerminalCapabilities, TerminalSize};
use oci_distribution::manifest::{OciImageManifest, OciManifest};
use policy_evaluator::{
    constants::*, policy_evaluator::PolicyExecutionMode, policy_metadata::Metadata,
};
use prettytable::{format::FormatBuilder, Table};
use pulldown_cmark::{Options, Parser};
use std::convert::TryFrom;
use syntect::parsing::SyntaxSet;

pub(crate) async fn inspect(
    uri: &str,
    output: OutputType,
    sources: Option<Sources>,
    docker_config: Option<DockerConfig>,
) -> Result<()> {
    let uri = crate::utils::map_path_to_uri(uri)?;
    let wasm_path = crate::utils::wasm_path(uri.as_str())?;
    let printer = get_printer(&output);

    let metadata = Metadata::from_path(&wasm_path)
        .map_err(|e| anyhow!("Error parsing policy metadata: {}", e))?;

    let signatures = fetch_signatures_manifest(uri.as_str(), sources, docker_config).await;

    match metadata {
        Some(metadata) => printer.print(&metadata)?,
        None => return Err(anyhow!(
            "No Kubewarden metadata found inside of '{}'.\nPolicies can be annotated with the `kwctl annotate` command.",
            uri
        )),
    };

    if let Some(signatures) = signatures {
        println!();
        println!("Sigstore signatures");
        println!();
        let sigstore_printer = get_signatures_printer(output);
        sigstore_printer.print(&signatures);
    }

    Ok(())
}

pub(crate) enum OutputType {
    Yaml,
    Pretty,
}

impl TryFrom<Option<&str>> for OutputType {
    type Error = anyhow::Error;

    fn try_from(value: Option<&str>) -> Result<Self, Self::Error> {
        match value {
            Some("yaml") => Ok(Self::Yaml),
            None => Ok(Self::Pretty),
            Some(unknown) => Err(anyhow!("Invalid output format '{}'", unknown)),
        }
    }
}

fn get_printer(output_type: &OutputType) -> Box<dyn MetadataPrinter> {
    match output_type {
        OutputType::Yaml => Box::new(MetadataYamlPrinter {}),
        OutputType::Pretty => Box::new(MetadataPrettyPrinter {}),
    }
}

trait MetadataPrinter {
    fn print(&self, metadata: &Metadata) -> Result<()>;
}

struct MetadataYamlPrinter {}

impl MetadataPrinter for MetadataYamlPrinter {
    fn print(&self, metadata: &Metadata) -> Result<()> {
        let metadata_yaml = serde_yaml::to_string(&metadata)?;
        println!("{}", metadata_yaml);
        Ok(())
    }
}

struct MetadataPrettyPrinter {}

impl MetadataPrettyPrinter {
    fn annotation_to_row_key(&self, text: &str) -> String {
        let mut out = String::from(text);
        out.push(':');
        String::from(out.trim_start_matches("io.kubewarden.policy."))
    }

    fn print_metadata_generic_info(&self, metadata: &Metadata) -> Result<()> {
        let protocol_version = metadata
            .protocol_version
            .clone()
            .ok_or_else(|| anyhow!("Invalid policy: protocol_version not defined"))?;

        let pretty_annotations = vec![
            KUBEWARDEN_ANNOTATION_POLICY_TITLE,
            KUBEWARDEN_ANNOTATION_POLICY_DESCRIPTION,
            KUBEWARDEN_ANNOTATION_POLICY_AUTHOR,
            KUBEWARDEN_ANNOTATION_POLICY_URL,
            KUBEWARDEN_ANNOTATION_POLICY_SOURCE,
            KUBEWARDEN_ANNOTATION_POLICY_LICENSE,
        ];
        let mut annotations = metadata.annotations.clone().unwrap_or_default();

        let mut table = Table::new();
        table.set_format(FormatBuilder::new().padding(0, 1).build());

        table.add_row(row![Fmbl -> "Details"]);
        for annotation in pretty_annotations.iter() {
            if let Some(value) = annotations.get(&String::from(*annotation)) {
                table.add_row(row![Fgbl -> self.annotation_to_row_key(annotation), d -> value]);
                annotations.remove(&String::from(*annotation));
            }
        }
        table.add_row(row![Fgbl -> "mutating:", metadata.mutating]);
        table.add_row(row![Fgbl -> "context aware:", metadata.context_aware]);
        table.add_row(row![Fgbl -> "execution mode:", metadata.execution_mode]);
        if metadata.execution_mode == PolicyExecutionMode::KubewardenWapc {
            table.add_row(row![Fgbl -> "protocol version:", protocol_version]);
        }

        let _usage = annotations.remove(KUBEWARDEN_ANNOTATION_POLICY_USAGE);
        if !annotations.is_empty() {
            table.add_row(row![]);
            table.add_row(row![Fmbl -> "Annotations"]);
            for (annotation, value) in annotations.iter() {
                table.add_row(row![Fgbl -> annotation, d -> value]);
            }
        }
        table.printstd();

        Ok(())
    }

    fn print_metadata_rules(&self, metadata: &Metadata) -> Result<()> {
        let rules_yaml = serde_yaml::to_string(&metadata.rules)?;

        // Quick hack to print a colorized "Rules" section, with the same
        // style as the other sections we print
        let mut table = Table::new();
        table.set_format(FormatBuilder::new().padding(0, 1).build());
        table.add_row(row![Fmbl -> "Rules"]);
        table.printstd();

        let text = format!("```yaml\n{}```", rules_yaml);
        self.render_markdown(&text)
    }

    fn print_metadata_usage(&self, metadata: &Metadata) -> Result<()> {
        let usage = match metadata.annotations.clone() {
            None => None,
            Some(annotations) => annotations
                .get(KUBEWARDEN_ANNOTATION_POLICY_USAGE)
                .map(String::from),
        };

        if usage.is_none() {
            return Ok(());
        }

        // Quick hack to print a colorized "Rules" section, with the same
        // style as the other sections we print
        let mut table = Table::new();
        table.set_format(FormatBuilder::new().padding(0, 1).build());
        table.add_row(row![Fmbl -> "Usage"]);
        table.printstd();

        self.render_markdown(&usage.unwrap())
    }

    fn render_markdown(&self, text: &str) -> Result<()> {
        let size = TerminalSize::detect().unwrap_or_default();
        let columns = size.columns;
        let settings = mdcat::Settings {
            terminal_capabilities: TerminalCapabilities::detect(),
            terminal_size: TerminalSize { columns, ..size },
            resource_access: ResourceAccess::LocalOnly,
            syntax_set: SyntaxSet::load_defaults_newlines(),
        };
        let parser = Parser::new_ext(
            text,
            Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH,
        );
        let env = mdcat::Environment::for_local_directory(&std::env::current_dir()?)?;

        let stdout = std::io::stdout();
        let mut output = stdout.lock();
        mdcat::push_tty(&settings, &env, &mut output, parser).or_else(|error| {
            if error.kind() == std::io::ErrorKind::BrokenPipe {
                Ok(())
            } else {
                Err(anyhow!("Cannot render markdown to stdout: {:?}", error))
            }
        })
    }
}

impl MetadataPrinter for MetadataPrettyPrinter {
    fn print(&self, metadata: &Metadata) -> Result<()> {
        self.print_metadata_generic_info(metadata)?;
        println!();
        self.print_metadata_rules(metadata)?;
        println!();
        self.print_metadata_usage(metadata)
    }
}

fn get_signatures_printer(output_type: OutputType) -> Box<dyn SignaturesPrinter> {
    match output_type {
        OutputType::Yaml => Box::new(SignaturesYamlPrinter {}),
        OutputType::Pretty => Box::new(SignaturesPrettyPrinter {}),
    }
}

trait SignaturesPrinter {
    fn print(&self, signatures: &OciImageManifest);
}

struct SignaturesPrettyPrinter {}

impl SignaturesPrinter for SignaturesPrettyPrinter {
    fn print(&self, signatures: &OciImageManifest) {
        for layer in &signatures.layers {
            let mut table = Table::new();
            table.set_format(FormatBuilder::new().padding(0, 1).build());
            table.add_row(row![Fmbl -> "Digest: ", layer.digest]);
            table.add_row(row![Fmbl -> "Media type: ", layer.media_type]);
            table.add_row(row![Fmbl -> "Size: ", layer.size]);
            if let Some(annotations) = &layer.annotations {
                table.add_row(row![Fmbl -> "Annotations"]);
                for annotation in annotations.iter() {
                    table.add_row(row![Fgbl -> annotation.0, annotation.1]);
                }
            }
            table.printstd();
            println!();
        }
    }
}

struct SignaturesYamlPrinter {}

impl SignaturesPrinter for SignaturesYamlPrinter {
    fn print(&self, signatures: &OciImageManifest) {
        let signatures_yaml = serde_yaml::to_string(signatures);
        if let Ok(signatures_yaml) = signatures_yaml {
            println!("{}", signatures_yaml)
        }
    }
}

async fn fetch_signatures_manifest(
    uri: &str,
    sources: Option<Sources>,
    docker_config: Option<DockerConfig>,
) -> Option<OciImageManifest> {
    let registry = Registry::new(docker_config.as_ref());
    let digest = registry.manifest_digest(uri, sources.as_ref()).await.ok()?;
    let signature_url = get_signature_url(uri.to_string(), digest.as_str())?;
    let manifest = registry
        .manifest(signature_url.as_str(), sources.as_ref())
        .await
        .ok()?;

    match manifest {
        OciManifest::Image(img) => Some(img),
        _ => None,
    }
}

// simulate cosign triangulate output. Use the manifest digest, which returns the sha256 of the manifest content, then replace ':' with '-' and append '.sig'
fn get_signature_url(mut uri: String, digest: &str) -> Option<String> {
    let mut signatures_tag = digest.replace(':', "-");
    signatures_tag.push_str(".sig");
    let last_colon_index = uri.rfind(':')?;
    uri.replace_range(last_colon_index + 1.., signatures_tag.as_str());

    Some(uri)
}

#[cfg(test)]
mod tests {
    use super::*;

    use rstest::rstest;

    #[rstest]
    #[case("registry://ghcr.io/kubewarden/tests/pod-privileged:v0.1.9",
    "sha256:0d6611ea12cf2904066308dde1c480b5d4f40e19b12f51f101a256b44d6c2dd5",
    Some(String::from("registry://ghcr.io/kubewarden/tests/pod-privileged:sha256-0d6611ea12cf2904066308dde1c480b5d4f40e19b12f51f101a256b44d6c2dd5.sig")))]
    #[case("ghcr.io/kubewarden/tests/pod-privileged:v0.1.9",
    "sha256:0d6611ea12cf2904066308dde1c480b5d4f40e19b12f51f101a256b44d6c2dd5",
    Some(String::from("ghcr.io/kubewarden/tests/pod-privileged:sha256-0d6611ea12cf2904066308dde1c480b5d4f40e19b12f51f101a256b44d6c2dd5.sig")))]
    #[case(
        "not_valid",
        "sha256:0d6611ea12cf2904066308dde1c480b5d4f40e19b12f51f101a256b44d6c2dd5",
        None
    )]
    fn test_get_signature_url(
        #[case] input_url: &str,
        #[case] input_digest: &str,
        #[case] expected: Option<String>,
    ) {
        assert_eq!(
            get_signature_url(input_url.to_string(), input_digest),
            expected
        )
    }
}
