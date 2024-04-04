use std::collections::HashMap;

use log::{debug, error};
use lsp_types::{Position, Url};
use yaml_rust::{Yaml, YamlLoader};

use super::{
    git, treesitter, GitlabElement, GitlabFile, IncludeInformation, NodeDefinition, ParseResults,
};

pub trait Parser {
    fn get_all_extends(
        &self,
        uri: String,
        content: &str,
        extend_name: Option<&str>,
    ) -> Vec<GitlabElement>;
    fn get_all_job_needs(
        &self,
        uri: String,
        content: &str,
        extend_name: Option<&str>,
    ) -> Vec<GitlabElement>;
    fn get_all_stages(&self, uri: String, content: &str) -> Vec<GitlabElement>;
    fn get_position_type(&self, content: &str, position: Position) -> PositionType;
    fn get_root_node(&self, uri: &str, content: &str, node_key: &str) -> Option<GitlabElement>;
    fn parse_contents(&self, uri: &Url, content: &str, _follow: bool) -> Option<ParseResults>;
    fn parse_contents_recursive(
        &self,
        parse_results: &mut ParseResults,
        uri: &lsp_types::Url,
        content: &str,
        _follow: bool,
        iteration: i32,
    ) -> Option<()>;
}

#[allow(clippy::module_name_repetitions)]
pub struct ParserImpl {
    treesitter: Box<dyn treesitter::Treesitter>,
    git: Box<dyn git::Git>,
}

// TODO: rooot for the case of importing f9
pub enum PositionType {
    Extend,
    Stage,
    Variable,
    None,
    RootNode,
    Include(IncludeInformation),
    Needs(NodeDefinition),
}

impl ParserImpl {
    pub fn new(
        remote_urls: Vec<String>,
        package_map: HashMap<String, String>,
        cache_path: String,
        treesitter: Box<dyn treesitter::Treesitter>,
    ) -> ParserImpl {
        ParserImpl {
            treesitter,
            git: Box::new(git::GitImpl::new(remote_urls, package_map, cache_path)),
        }
    }

    fn parse_remote_files(&self, parse_results: &mut ParseResults, remote_files: &[GitlabFile]) {
        for remote_file in remote_files {
            parse_results.nodes.append(
                &mut self
                    .treesitter
                    .get_all_root_nodes(remote_file.path.as_str(), remote_file.content.as_str()),
            );

            parse_results.files.push(remote_file.clone());

            // arrays are overriden in gitlab.
            let found_stages = self
                .treesitter
                .get_stage_definitions(remote_file.path.as_str(), remote_file.content.as_str());

            if !found_stages.is_empty() {
                parse_results.stages = found_stages;
            }

            parse_results.variables.append(
                &mut self
                    .treesitter
                    .get_root_variables(remote_file.path.as_str(), remote_file.content.as_str()),
            );
        }
    }
}

impl Parser for ParserImpl {
    fn get_all_extends(
        &self,
        uri: String,
        content: &str,
        extend_name: Option<&str>,
    ) -> Vec<GitlabElement> {
        self.treesitter.get_all_extends(uri, content, extend_name)
    }

    fn get_all_stages(&self, uri: String, content: &str) -> Vec<GitlabElement> {
        self.treesitter.get_all_stages(uri, content)
    }

    fn get_position_type(&self, content: &str, position: Position) -> PositionType {
        self.treesitter.get_position_type(content, position)
    }

    fn get_root_node(&self, uri: &str, content: &str, node_key: &str) -> Option<GitlabElement> {
        self.treesitter.get_root_node(uri, content, node_key)
    }

    fn parse_contents(&self, uri: &Url, content: &str, follow: bool) -> Option<ParseResults> {
        let files: Vec<GitlabFile> = vec![];
        let nodes: Vec<GitlabElement> = vec![];
        let stages: Vec<GitlabElement> = vec![];
        let variables: Vec<GitlabElement> = vec![];

        let mut parse_results = ParseResults {
            files,
            nodes,
            stages,
            variables,
        };

        self.parse_contents_recursive(&mut parse_results, uri, content, follow, 0)?;

        Some(parse_results)
    }

    #[allow(clippy::too_many_lines)]
    fn parse_contents_recursive(
        &self,
        parse_results: &mut ParseResults,
        uri: &lsp_types::Url,
        content: &str,
        follow: bool,
        iteration: i32,
    ) -> Option<()> {
        // #safety wow amazed
        if iteration > 10 {
            return None;
        }

        parse_results.files.push(GitlabFile {
            path: uri.as_str().into(),
            content: content.into(),
        });

        parse_results
            .nodes
            .append(&mut self.treesitter.get_all_root_nodes(uri.as_str(), content));

        parse_results
            .variables
            .append(&mut self.treesitter.get_root_variables(uri.as_str(), content));

        // arrays are overriden in gitlab.
        let found_stages = self.treesitter.get_stage_definitions(uri.as_str(), content);
        if !found_stages.is_empty() {
            parse_results.stages = found_stages;
        }

        let element = self
            .treesitter
            .get_root_node(uri.as_str(), content, "include")?;

        let documents = YamlLoader::load_from_str(element.content?.as_str()).ok()?;
        let content = &documents[0];

        if let Yaml::Hash(include_root) = content {
            for (_, root) in include_root {
                if let Yaml::Array(includes) = root {
                    for include in includes {
                        if let Yaml::Hash(item) = include {
                            let mut remote_pkg = String::new();
                            let mut remote_tag = String::new();
                            let mut remote_files: Vec<String> = vec![];

                            for (key, item_value) in item {
                                if follow && !remote_pkg.is_empty() {
                                    let remote_files = match self.git.fetch_remote_repository(
                                        remote_pkg.as_str(),
                                        remote_tag.as_str(),
                                        &remote_files,
                                    ) {
                                        Ok(rf) => rf,
                                        Err(err) => {
                                            error!("error retrieving remote files: {}", err);

                                            vec![]
                                        }
                                    };

                                    self.parse_remote_files(parse_results, &remote_files);
                                }

                                if let Yaml::String(key_str) = key {
                                    match key_str.trim().to_lowercase().as_str() {
                                        "local" => {
                                            if let Yaml::String(value) = item_value {
                                                let current_uri = uri.join(value.as_str()).ok()?;
                                                let current_content =
                                                    std::fs::read_to_string(current_uri.path())
                                                        .ok()?;

                                                if follow {
                                                    self.parse_contents_recursive(
                                                        parse_results,
                                                        &current_uri,
                                                        &current_content,
                                                        follow,
                                                        iteration + 1,
                                                    );
                                                }
                                            }
                                        }
                                        "remote" => {
                                            if let Yaml::String(value) = item_value {
                                                let remote_url = match Url::parse(value) {
                                                    Ok(f) => f,
                                                    Err(err) => {
                                                        error!("could not parse remote URL: {}; got err: {:?}", value, err);
                                                        continue;
                                                    }
                                                };

                                                let file = match self
                                                    .git
                                                    .fetch_remote(remote_url.clone())
                                                {
                                                    Ok(res) => res,
                                                    Err(err) => {
                                                        error!("error retrieving remote file: {}; got err: {:?}", remote_url, err);
                                                        continue;
                                                    }
                                                };

                                                self.parse_remote_files(parse_results, &[file]);
                                            }
                                        }
                                        "project" => {
                                            if let Yaml::String(value) = item_value {
                                                remote_pkg = value.clone();
                                            }
                                        }
                                        "ref" => {
                                            if let Yaml::String(value) = item_value {
                                                remote_tag = value.clone();
                                            }
                                        }
                                        "file" => {
                                            debug!("files: {:?}", item_value);
                                            if let Yaml::Array(value) = item_value {
                                                for yml in value {
                                                    if let Yaml::String(p) = yml {
                                                        remote_files.push(p.to_string());
                                                    }
                                                }
                                            }
                                        }
                                        _ => break,
                                    }
                                }
                            }

                            if follow && !remote_pkg.is_empty() {
                                let remote_files = match self.git.fetch_remote_repository(
                                    remote_pkg.as_str(),
                                    remote_tag.as_str(),
                                    &remote_files,
                                ) {
                                    Ok(rf) => rf,
                                    Err(err) => {
                                        error!("error retrieving remote files: {}", err);

                                        vec![]
                                    }
                                };

                                self.parse_remote_files(parse_results, &remote_files);
                            }
                        }
                    }
                }
            }
        }

        Some(())
    }

    fn get_all_job_needs(
        &self,
        uri: String,
        content: &str,
        needs_name: Option<&str>,
    ) -> Vec<GitlabElement> {
        self.treesitter.get_all_job_needs(uri, content, needs_name)
    }
}