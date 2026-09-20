use rustc_hash::FxHashMap;

use crate::error::SpecError;

/// A compiled path tree for fast O(n) path matching without regex.
///
/// Each OpenAPI path template like `/users/{id}/posts/{postId}` is split into
/// segments and inserted into a trie. At match time we walk the trie segment by
/// segment, collecting captured path parameter values along the way.
#[derive(Debug, Default)]
pub struct PathTree {
    root: TrieNode,
}

#[derive(Debug, Default)]
struct TrieNode {
    /// Static children keyed by exact segment text.
    static_children: FxHashMap<String, TrieNode>,
    /// Dynamic child (a `{param}` segment). At most one per node.
    param_child: Option<Box<ParamChild>>,
    /// If this node is a terminal, the original template string.
    template: Option<String>,
}

#[derive(Debug)]
struct ParamChild {
    name: String,
    node: TrieNode,
}

/// Result of a successful path match.
pub struct PathMatch<'a> {
    /// The original OpenAPI path template that matched.
    pub template: &'a str,
    /// Captured path parameter name-value pairs.
    pub params: Vec<(&'a str, &'a str)>,
}

impl PathTree {
    /// Insert a path template into the tree.
    ///
    /// Templates use `{paramName}` for dynamic segments.
    pub fn insert(&mut self, template: &str) -> Result<(), SpecError> {
        let segments = split_segments(template);
        let mut node = &mut self.root;

        for segment in &segments {
            if let Some(param_name) = parse_param_segment(segment) {
                // Validate param name is non-empty
                if param_name.is_empty() {
                    return Err(SpecError::ParseError(format!(
                        "Empty path parameter name in template '{template}'"
                    )));
                }
                // Create or traverse into the param child
                let child = node.param_child.get_or_insert_with(|| {
                    Box::new(ParamChild {
                        name: param_name.to_string(),
                        node: TrieNode::default(),
                    })
                });
                node = &mut child.node;
            } else {
                node = node
                    .static_children
                    .entry(segment.to_string())
                    .or_default();
            }
        }

        node.template = Some(template.to_string());
        Ok(())
    }

    /// Match a request path against the tree.
    ///
    /// Returns the matched template and captured path parameters, or `None`.
    ///
    /// Per the OpenAPI spec and RFC 3986, path parameter values MUST NOT contain
    /// unescaped forward slashes (`/`), question marks (`?`), or hashes (`#`).
    /// Since we split on `/` and the query string is stripped before matching,
    /// this is enforced structurally.
    pub fn match_path<'a>(&'a self, path: &'a str) -> Option<PathMatch<'a>> {
        let segments = split_segments(path);
        let mut params = Vec::new();
        let node = self.walk(&self.root, &segments, &mut params)?;

        node.template
            .as_ref()
            .map(|t| PathMatch {
                template: t.as_str(),
                params,
            })
    }

    fn walk<'a>(
        &'a self,
        node: &'a TrieNode,
        segments: &[&'a str],
        params: &mut Vec<(&'a str, &'a str)>,
    ) -> Option<&'a TrieNode> {
        let Some((segment, rest)) = segments.split_first() else {
            return Some(node);
        };

        // Try static match first (higher priority than param match per OpenAPI spec)
        if let Some(child) = node.static_children.get(*segment) {
            let saved_len = params.len();
            if let Some(result) = self.walk(child, rest, params) {
                if result.template.is_some() {
                    return Some(result);
                }
            }
            params.truncate(saved_len);
        }

        // Try param match
        if let Some(ref child) = node.param_child {
            // RFC 3986: param values must not contain unescaped /, ?, #
            // / is already excluded by segment splitting.
            // ? and # should have been stripped before matching, but guard anyway.
            if !segment.contains('?') && !segment.contains('#') && !segment.is_empty() {
                params.push((child.name.as_str(), segment));
                if let Some(result) = self.walk(&child.node, rest, params) {
                    if result.template.is_some() {
                        return Some(result);
                    }
                }
                params.pop();
            }
        }

        None
    }
}

/// Split a path into segments, filtering out empty segments.
/// `/users/{id}/posts` → `["users", "{id}", "posts"]`
fn split_segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

/// If a segment is `{paramName}`, return `Some("paramName")`.
fn parse_param_segment(segment: &str) -> Option<&str> {
    if segment.starts_with('{') && segment.ends_with('}') {
        Some(&segment[1..segment.len() - 1])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_tree(templates: &[&str]) -> PathTree {
        let mut tree = PathTree::default();
        for t in templates {
            tree.insert(t).unwrap();
        }
        tree
    }

    #[test]
    fn test_simple_static_path() {
        let tree = build_tree(&["/users"]);
        let m = tree.match_path("/users").unwrap();
        assert_eq!(m.template, "/users");
        assert!(m.params.is_empty());
    }

    #[test]
    fn test_no_match() {
        let tree = build_tree(&["/users"]);
        assert!(tree.match_path("/posts").is_none());
    }

    #[test]
    fn test_path_with_param() {
        let tree = build_tree(&["/users/{id}"]);
        let m = tree.match_path("/users/123").unwrap();
        assert_eq!(m.template, "/users/{id}");
        assert_eq!(m.params, vec![("id", "123")]);
    }

    #[test]
    fn test_multiple_params() {
        let tree = build_tree(&["/users/{userId}/posts/{postId}"]);
        let m = tree.match_path("/users/42/posts/99").unwrap();
        assert_eq!(m.template, "/users/{userId}/posts/{postId}");
        assert_eq!(m.params, vec![("userId", "42"), ("postId", "99")]);
    }

    #[test]
    fn test_no_match_extra_segments() {
        let tree = build_tree(&["/users/{id}"]);
        assert!(tree.match_path("/users/123/extra").is_none());
    }

    #[test]
    fn test_no_match_too_few_segments() {
        let tree = build_tree(&["/users/{id}"]);
        assert!(tree.match_path("/users").is_none());
    }

    #[test]
    fn test_static_over_param_priority() {
        // Static segments have higher priority than param segments
        let tree = build_tree(&["/users/me", "/users/{id}"]);
        let m = tree.match_path("/users/me").unwrap();
        assert_eq!(m.template, "/users/me");
        assert!(m.params.is_empty());

        let m = tree.match_path("/users/123").unwrap();
        assert_eq!(m.template, "/users/{id}");
        assert_eq!(m.params, vec![("id", "123")]);
    }

    #[test]
    fn test_multiple_routes() {
        let tree = build_tree(&["/users", "/users/{id}", "/posts", "/posts/{id}/comments"]);

        assert!(tree.match_path("/users").is_some());
        assert!(tree.match_path("/users/5").is_some());
        assert!(tree.match_path("/posts").is_some());
        assert!(tree.match_path("/posts/1/comments").is_some());
        assert!(tree.match_path("/other").is_none());
    }

    #[test]
    fn test_trailing_slash() {
        let tree = build_tree(&["/users"]);
        // Trailing slash results in an empty segment which is filtered out
        assert!(tree.match_path("/users/").is_some());
    }

    #[test]
    fn test_param_rejects_empty_segment() {
        let tree = build_tree(&["/users/{id}/posts"]);
        // /users//posts has an empty segment where {id} is — should not match
        assert!(tree.match_path("/users//posts").is_none());
    }

    #[test]
    fn test_root_path() {
        let tree = build_tree(&["/"]);
        assert!(tree.match_path("/").is_some());
    }

    #[test]
    fn test_param_no_question_mark() {
        // ? in a path segment would be malformed (should be stripped before matching)
        let tree = build_tree(&["/users/{id}"]);
        assert!(tree.match_path("/users/12?3").is_none());
    }
}
