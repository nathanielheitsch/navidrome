//! Port of `persistence/sql_restful.go` (177L) — the rest-options-to-query
//! shaping layer of the base repository.
//!
//! [`RestRepository`] carries the `sqlRepository` fields this layer reads:
//! `tableName`, `filterMappings` (`sql_base_repository.go:45,111`) and
//! `isFieldWhiteListed` (`:46,110` — registered via `registerModelWhiteList`,
//! ported here as [`RestRepository::with_field_white_list`]). Go's
//! package-global `whiteList` map + `mutex` (`:142-145`) exists only because
//! Go captures the whitelist closure at `registerModel` time and other
//! packages mutate the global from `init()`; Rust owns the set by value in
//! the instance, so the registry/mutex have no counterpart.
//!
//! `fullTextFilter` (`sql_restful.go:74-82`) is deferred: it composes
//! `mbidExpr` and `getSearchStrategy` from `persistence/sql_search.go`, which
//! is another lane's module (`sql_search*.rs` stubs), and nothing in this
//! layer can stand in for them; it ports with that slice. There is no logging
//! dependency here, so Go's `log.Warn` calls (`:29,47`) drop silently with
//! the same filter/sort-dropping semantics.
//!
//! Determinism deviation: Go iterates `options.Filters` (a map) in random
//! order, so the AND-clause order was never observable-contract; the port
//! takes `BTreeMap` values and the tests pin the sorted order.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::sqlrepo::Value;

/// The `model.Annotations{}` fields `registerFieldWhiteList` always merges
/// into every whitelist (`sql_restful.go:151-154`), snake_cased.
pub const ANNOTATION_FIELDS: &[&str] = &[
    "play_count",
    "play_date",
    "rating",
    "rated_at",
    "starred",
    "starred_at",
    "average_rating",
];

/// One rest-layer WHERE clause — the `Sqlizer`s the filter functions build
/// (`sql_restful.go:50-118`): squirrel `Eq` / `Like` / free-form fragment.
#[derive(Debug, Clone, PartialEq)]
pub enum Filter {
    /// `column = ?` (squirrel `Eq`, `eqFilter` `:86-88`).
    Eq {
        /// The matched column.
        column: String,
        /// The bound value.
        value: Value,
    },
    /// `column LIKE ?` (squirrel `Like`).
    Like {
        /// The matched column.
        column: String,
        /// The bound pattern (already carries its `%` wildcards).
        pattern: String,
    },
    /// Caller-built fragment with its bound args (squirrel `Expr`-based
    /// composites such as `substringFilter`).
    Raw {
        /// The fragment SQL.
        sql: String,
        /// The fragment's bound values.
        raw_args: Vec<Value>,
    },
}

impl Filter {
    /// Renders this clause and pushes its bound values onto `args`.
    fn render(&self, args: &mut Vec<Value>) -> String {
        match self {
            Self::Eq { column, value } => {
                args.push(value.clone());
                format!("{column} = ?")
            }
            Self::Like { column, pattern } => {
                args.push(Value::from(pattern.as_str()));
                format!("{column} LIKE ?")
            }
            Self::Raw { sql, raw_args } => {
                args.extend(raw_args.iter().cloned());
                sql.clone()
            }
        }
    }
}

/// A `filterFunc` (`sql_restful.go:11`): maps `(field, value)` to a filter,
/// or `None` to drop it (Go's nil-`Sqlizer` check, `:24-26`).
pub type FilterFn = Box<dyn Fn(&str, &str) -> Option<Filter>>;

/// `rest.QueryOptions` input (`github.com/deluan/rest`), the shape
/// `parseRestOptions` consumes (`sql_restful.go:31-45`).
#[derive(Debug, Clone, Default)]
pub struct RestQueryOptions {
    /// Sort field name (camelCase from the API, snake_cased in `sanitizeSort`).
    pub sort: String,
    /// `asc`/`desc` direction.
    pub order: String,
    /// Page size (`Max`).
    pub max: i64,
    /// Page start (`Offset`).
    pub offset: i64,
    /// Filter values by field name; empty values are ignored (`:22-24`).
    pub filters: BTreeMap<String, String>,
}

/// `model.QueryOptions` output of `parseRestOptions`
/// (`model/query_options.go` as used by `sql_restful.go:33-43`): the
/// sanitized sort plus the parsed filter clauses.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QueryOptions {
    /// Sanitized sort field (`sanitizeSort`, empty when dropped).
    pub sort: String,
    /// Sanitized direction (`asc`/`desc`, empty when unset).
    pub order: String,
    /// Page size.
    pub max: i64,
    /// Page start.
    pub offset: i64,
    /// The `"seed"` filter value `parseRestOptions` lifts out of the map
    /// (`:38-40`).
    pub seed: Option<String>,
    /// The parsed WHERE clauses (`None` when no filters were given, `:44`).
    pub filters: Option<Vec<Filter>>,
}

impl QueryOptions {
    /// Renders `filters` as the `WHERE` body `applyFilters` feeds into the
    /// builder (`sql_base_repository.go:60-70`): every clause AND-joined,
    /// values bound in order. `None` when there are no filters.
    #[must_use]
    pub fn where_clause(&self) -> Option<(String, Vec<Value>)> {
        let filters = self.filters.as_ref()?;
        if filters.is_empty() {
            return None;
        }
        let mut args = Vec::new();
        let sql = filters
            .iter()
            .map(|f| f.render(&mut args))
            .collect::<Vec<_>>()
            .join(" AND ");
        Some((sql, args))
    }
}

/// `toSnakeCase` (`persistence/helpers.go:47-53`): `matchFirstCap`
/// (`(.)([A-Z][a-z]+)` → `${1}_${2}`) then `matchAllCap`
/// (`([a-z0-9])([A-Z])` → `${1}_${2}`), lowercased. Ported without the
/// regexes: an uppercase char is prefixed with `_` when its successor is
/// lowercase (the `[A-Z][a-z]+` split) or its predecessor is a lowercase
/// letter/digit (the `[a-z0-9]` split); a leading uppercase never gets one.
#[must_use]
pub fn to_snake_case(s: &str) -> String {
    let chars = s.chars().collect::<Vec<_>>();
    let mut out = String::with_capacity(s.len() + 4);
    for (i, c) in chars.iter().enumerate() {
        if c.is_uppercase() {
            let next_is_lower = chars.get(i + 1).is_some_and(|next| next.is_lowercase());
            let prev_is_lower_or_digit =
                i > 0 && (chars[i - 1].is_lowercase() || chars[i - 1].is_ascii_digit());
            if i > 0 && (next_is_lower || prev_is_lower_or_digit) {
                out.push('_');
            }
        }
        out.extend(c.to_lowercase());
    }
    out
}

/// `registerModelWhiteList`/`registerFieldWhiteList` (`sql_restful.go:121-135`)
/// minus the global registry: snake_case every instance field name and merge
/// [`ANNOTATION_FIELDS`] in.
#[must_use]
pub fn register_model_white_list(
    fields: impl IntoIterator<Item = impl AsRef<str>>,
) -> HashSet<String> {
    let mut set = HashSet::new();
    for field in fields {
        set.insert(to_snake_case(field.as_ref()));
    }
    for field in ANNOTATION_FIELDS {
        set.insert((*field).to_string());
    }
    set
}

/// The rest-facing half of `sqlRepository` (`sql_restful.go:10-16` uses):
/// `tableName`, `filterMappings`, `isFieldWhiteListed`.
#[derive(Default)]
pub struct RestRepository {
    table_name: String,
    filter_mappings: HashMap<String, FilterFn>,
    white_list: Option<HashSet<String>>,
}

impl RestRepository {
    /// Zero-value `sqlRepository` equivalent for this layer: a table name with
    /// no filter mappings and no whitelist (`isFieldWhiteListed == nil`).
    #[must_use]
    pub fn new(table_name: impl Into<String>) -> Self {
        Self {
            table_name: table_name.into(),
            filter_mappings: HashMap::new(),
            white_list: None,
        }
    }

    /// The `tableName` this layer renders into filters (`registerModel`,
    /// `sql_base_repository.go:101-106`).
    #[must_use]
    pub fn table_name(&self) -> &str {
        &self.table_name
    }

    /// Sets `filterMappings` (`registerModel(instance, filters)`,
    /// `sql_base_repository.go:111`).
    #[must_use]
    pub fn with_filter_mappings(
        mut self,
        mappings: impl IntoIterator<Item = (impl Into<String>, FilterFn)>,
    ) -> Self {
        self.filter_mappings = mappings
            .into_iter()
            .map(|(name, func)| (name.into(), func))
            .collect();
        self
    }

    /// Sets `isFieldWhiteListed` via `registerModelWhiteList`
    /// (`sql_base_repository.go:110`, `sql_restful.go:110`).
    #[must_use]
    pub fn with_field_white_list(
        mut self,
        fields: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Self {
        self.white_list = Some(register_model_white_list(fields));
        self
    }

    /// `sanitizeSort` (`sql_restful.go:47-60`): snake_case the sort field,
    /// drop it unless whitelisted (`lookupSortMapping` is the base
    /// repository's slice; its known-mapping arm joins there), and fold the
    /// order to `asc`/`desc` (empty stays empty, `:55-59`).
    #[must_use]
    pub fn sanitize_sort(&self, sort: &str, order: &str) -> (String, String) {
        let mut sort = sort;
        let mut sort_out = String::new();
        if !sort.is_empty() {
            let snake = to_snake_case(sort);
            let known_sort = self
                .white_list
                .as_ref()
                .is_some_and(|list| list.contains(&snake));
            if known_sort {
                sort_out = snake;
            }
            sort = "";
        }
        let _ = sort;
        let mut order_out = String::new();
        if !order.is_empty() {
            order_out = order.to_lowercase();
            if order_out != "desc" {
                order_out = "asc".to_string();
            }
        }
        (sort_out, order_out)
    }

    /// `parseRestFilters` (`sql_restful.go:13-30`): walk the filter values,
    /// skip empties, consult the custom mappings, enforce the whitelist,
    /// exact-match `…id` fields and "starts with" everything else. `None`
    /// when nothing survived (Go returns the nil `And{}`).
    #[must_use]
    pub fn parse_rest_filters(&self, options: &RestQueryOptions) -> Option<Vec<Filter>> {
        if options.filters.is_empty() {
            return None;
        }
        let mut filters = Vec::new();
        for (field, value) in &options.filters {
            // Ignore filters with empty values (`:23-25`).
            if value.is_empty() {
                continue;
            }
            // Look for a custom filter function (`:27-31`).
            let field = field.to_lowercase();
            if let Some(func) = self.filter_mappings.get(&field) {
                if let Some(filter) = func(&field, value) {
                    filters.push(filter);
                }
                continue;
            }
            // Ignore invalid filters (not based on a field or filter
            // function, `:33-36`); no whitelist set means everything passes,
            // matching the Go nil-func check `:33`.
            if let Some(list) = &self.white_list {
                if !list.contains(&field) {
                    continue;
                }
            }
            // For fields ending in "id", use an exact match (`:38-41`).
            if field.ends_with("id") {
                filters.push(eq_filter(&field, value));
                continue;
            }
            // Default to a "starts with" filter (`:43-44`).
            filters.push(Filter::Like {
                column: field,
                pattern: format!("{value}%"),
            });
        }
        (!filters.is_empty()).then_some(filters)
    }

    /// `parseRestOptions` (`sql_restful.go:31-45`): sanitize sort/order,
    /// copy max/offset, lift `"seed"` out of the filters map, parse the rest.
    /// Go's variadic `options ...rest.QueryOptions` ports as `Option`.
    #[must_use]
    pub fn parse_rest_options(&self, options: Option<&RestQueryOptions>) -> QueryOptions {
        let Some(options) = options else {
            return QueryOptions::default();
        };
        let (sort, order) = self.sanitize_sort(&options.sort, &options.order);
        QueryOptions {
            sort,
            order,
            max: options.max,
            offset: options.offset,
            seed: options.filters.get("seed").cloned(),
            filters: self.parse_rest_filters(options),
        }
    }
}

/// `eqFilter` (`sql_restful.go:86-88`): `Eq{field: value}`.
#[must_use]
pub fn eq_filter(field: &str, value: &str) -> Filter {
    Filter::Eq {
        column: field.to_string(),
        value: Value::from(value),
    }
}

/// `startsWithFilter` (`sql_restful.go:90-94`): a `filterFunc` binding
/// `field LIKE 'value%'` — it ignores the requested field name (`:91`).
pub fn starts_with_filter(field: impl Into<String>) -> FilterFn {
    let column = field.into();
    Box::new(move |_field: &str, value: &str| {
        Some(Filter::Like {
            column: column.clone(),
            pattern: format!("{value}%"),
        })
    })
}

/// `containsFilter` (`sql_restful.go:96-100`): a `filterFunc` binding
/// `field LIKE '%value%'`.
pub fn contains_filter(field: impl Into<String>) -> FilterFn {
    let column = field.into();
    Box::new(move |_field: &str, value: &str| {
        Some(Filter::Like {
            column: column.clone(),
            pattern: format!("%{value}%"),
        })
    })
}

/// `booleanFilter` (`sql_restful.go:102-105`): `Eq{field: value == "true"}`
/// on the lowercased value.
#[must_use]
pub fn boolean_filter(field: &str, value: &str) -> Filter {
    Filter::Eq {
        column: field.to_string(),
        value: Value::Bool(value.to_lowercase() == "true"),
    }
}

/// `substringFilter` (`sql_restful.go:84-85`): every whitespace-separated
/// word of the value AND-joined as `field LIKE '%word%'`.
#[must_use]
pub fn substring_filter(field: &str, value: &str) -> Filter {
    let parts = value
        .split_whitespace()
        .map(|part| Filter::Like {
            column: field.to_string(),
            pattern: format!("%{part}%"),
        })
        .collect::<Vec<_>>();
    let mut args = Vec::new();
    let sql = parts
        .iter()
        .map(|f| f.render(&mut args))
        .collect::<Vec<_>>()
        .join(" AND ");
    Filter::Raw {
        sql,
        raw_args: args,
    }
}

/// `idFilter` (`sql_restful.go:106-108`): a `filterFunc` matching
/// `<table>.id` exactly, ignoring the requested field name.
pub fn id_filter(table: impl Into<String>) -> FilterFn {
    let column = format!("{}.id", table.into());
    Box::new(move |_field: &str, value: &str| {
        Some(Filter::Eq {
            column: column.clone(),
            value: Value::from(value),
        })
    })
}

/// `invalidFilter` (`sql_restful.go:110-114`): the always-false `Eq{"1": "0"}`
/// Go falls back to after its `log.Warn`.
#[must_use]
pub fn invalid_filter(field: &str, value: &str) -> Filter {
    let _ = (field, value);
    Filter::Eq {
        column: "1".to_string(),
        value: Value::from("0"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_snake_case_matches_helpers_go_shapes() {
        // Arrange/Act/Assert: matchFirstCap + matchAllCap (helpers.go:47-53).
        assert_eq!(to_snake_case("AlbumID"), "album_id");
        assert_eq!(to_snake_case("ApiKey"), "api_key");
        assert_eq!(to_snake_case("URL"), "url");
        assert_eq!(to_snake_case("PlayDate"), "play_date");
        assert_eq!(to_snake_case("aB"), "a_b");
        assert_eq!(to_snake_case("year"), "year");
    }

    #[test]
    fn sanitize_sort_snake_cases_whitelists_and_folds_order() {
        // Arrange: sanitizeSort (sql_restful.go:47-60) on a whitelist
        // registered via registerModelWhiteList (:121-135) + Annotations.
        let repo = RestRepository::new("album").with_field_white_list(["Title", "Year"]);
        // Act:
        let kept = repo.sanitize_sort("Title", "DESC");
        let dropped = repo.sanitize_sort("Artist", "");
        let empty_order = repo.sanitize_sort("Title", "");
        // Assert:
        assert_eq!(kept, ("title".to_string(), "desc".to_string()));
        assert_eq!(dropped, (String::new(), String::new()));
        assert_eq!(empty_order, ("title".to_string(), String::new()));
        // The annotation fields merged in (:151-154).
        let list = register_model_white_list(["Title"]);
        assert!(list.contains("starred"));
        assert!(list.contains("average_rating"));
    }

    #[test]
    fn parse_rest_filters_skips_empty_drops_unwhitelisted_and_specializes() {
        // Arrange: parseRestFilters (sql_restful.go:13-30) with mappings,
        // whitelist, an empty value, an "id" field and a default field.
        let repo = RestRepository::new("media_file")
            .with_filter_mappings([("genre", contains_filter("genre"))])
            .with_field_white_list(["Title", "ArtistID"]);
        let mut options = RestQueryOptions {
            filters: BTreeMap::new(),
            ..RestQueryOptions::default()
        };
        options
            .filters
            .insert("genre".to_string(), "jazz".to_string());
        options.filters.insert("title".to_string(), String::new());
        options
            .filters
            .insert("artist_id".to_string(), "ar-1".to_string());
        options
            .filters
            .insert("title2".to_string(), "The".to_string());
        options
            .filters
            .insert("unknown".to_string(), "x".to_string());
        // Act:
        let filters = repo.parse_rest_filters(&options).expect("filters survive");
        let (sql, args) = QueryOptions {
            filters: Some(filters),
            ..QueryOptions::default()
        }
        .where_clause()
        .expect("clauses render");
        // Assert: BTreeMap order (artist_id < genre < title2);
        // "unknown" is not whitelisted and "title2" snake-cases to title_2,
        // also not whitelisted, so both are dropped (:33-36).
        assert_eq!(sql, "artist_id = ? AND genre LIKE ?");
        assert_eq!(
            args,
            vec![
                Value::Text("ar-1".to_string()),
                Value::Text("%jazz%".to_string()),
            ]
        );
    }

    #[test]
    fn parse_rest_filters_none_when_map_empty() {
        // Arrange: the len(options.Filters)==0 branch (:14-16).
        let repo = RestRepository::new("album");
        // Act:
        let filters = repo.parse_rest_filters(&RestQueryOptions::default());
        // Assert:
        assert!(filters.is_none());
    }

    #[test]
    fn parse_rest_options_lifts_seed_and_sanitizes() {
        // Arrange: parseRestOptions (sql_restful.go:31-45).
        let repo = RestRepository::new("album").with_field_white_list(["Title"]);
        let mut options = RestQueryOptions {
            sort: "Title".to_string(),
            order: "random".to_string(),
            max: 20,
            offset: 40,
            ..RestQueryOptions::default()
        };
        options
            .filters
            .insert("seed".to_string(), "12345".to_string());
        options
            .filters
            .insert("title".to_string(), "Help".to_string());
        // Act:
        let qo = repo.parse_rest_options(Some(&options));
        let bare = repo.parse_rest_options(None);
        // Assert:
        assert_eq!(qo.sort, "title".to_string());
        assert_eq!(qo.order, "asc".to_string());
        assert_eq!(qo.max, 20);
        assert_eq!(qo.offset, 40);
        assert_eq!(qo.seed.as_deref(), Some("12345"));
        assert_eq!(
            qo.where_clause(),
            Some((
                "title LIKE ?".to_string(),
                vec![Value::Text("Help%".to_string())]
            ))
        );
        assert_eq!(bare, QueryOptions::default());
    }

    #[test]
    fn filter_helpers_render_go_shapes() {
        // Arrange: the standalone filter funcs (sql_restful.go:84-114).
        // Act/Assert: eqFilter (:86-88).
        let mut args = Vec::new();
        assert_eq!(
            eq_filter("id", "a1").render(&mut args),
            "id = ?".to_string()
        );
        // startsWithFilter (:90-94).
        let starts = starts_with_filter("sort_name")("name", "beat").expect("some");
        let mut args = Vec::new();
        assert_eq!(starts.render(&mut args), "sort_name LIKE ?".to_string());
        assert_eq!(args, vec![Value::Text("beat%".to_string())]);
        // containsFilter (:96-100).
        let contains = contains_filter("tags")("tags", "rock").expect("some");
        let mut args = Vec::new();
        assert_eq!(contains.render(&mut args), "tags LIKE ?".to_string());
        assert_eq!(args, vec![Value::Text("%rock%".to_string())]);
        // booleanFilter (:102-105).
        assert_eq!(
            boolean_filter("starred", "TRUE"),
            Filter::Eq {
                column: "starred".to_string(),
                value: Value::Bool(true)
            }
        );
        // substringFilter (:84-85).
        let sub = substring_filter("title", "britney  spears");
        let mut args = Vec::new();
        assert_eq!(
            sub.render(&mut args),
            "title LIKE ? AND title LIKE ?".to_string()
        );
        assert_eq!(
            args,
            vec![
                Value::Text("%britney%".to_string()),
                Value::Text("%spears%".to_string()),
            ]
        );
        // idFilter (:106-108).
        let idf = id_filter("artist")("whatever", "ar-9").expect("some");
        let mut args = Vec::new();
        assert_eq!(idf.render(&mut args), "artist.id = ?".to_string());
        assert_eq!(args, vec![Value::Text("ar-9".to_string())]);
        // invalidFilter (:110-114).
        let bad = invalid_filter("bogus", "value");
        let mut args = Vec::new();
        assert_eq!(bad.render(&mut args), "1 = ?".to_string());
        assert_eq!(args, vec![Value::Text("0".to_string())]);
    }
}
