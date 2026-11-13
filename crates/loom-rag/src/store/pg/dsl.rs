//! The statement shapes the stores repeat, as macros over the Diesel DSL: every table is
//! keyed by repository, so they differ only in table, columns and values.

/// `INSERT INTO t (cols) VALUES (vals) ON CONFLICT (keys) DO UPDATE SET col = excluded.col`
/// (`DO NOTHING` when no `on` clause is given).
macro_rules! upsert {
    ($tx:expr, $t:ident { $($c:ident : $v:expr),+ } on ($($k:ident),+) set [$($u:ident),+]) => {
        diesel::insert_into($t::table)
            .values(($($t::$c.eq($v)),+))
            .on_conflict(($($t::$k),+))
            .do_update()
            .set(($($t::$u.eq(diesel::upsert::excluded($t::$u))),+))
            .execute($tx)?
    };
    ($tx:expr, $t:ident { $($c:ident : $v:expr),+ }) => {
        diesel::insert_into($t::table)
            .values(($($t::$c.eq($v)),+))
            .on_conflict_do_nothing()
            .execute($tx)?
    };
}

/// `DELETE FROM t WHERE repo = $repo [AND col = $val]`.
macro_rules! remove {
    ($tx:expr, $t:ident, $repo:expr) => {
        diesel::delete($t::table.filter($t::repo.eq($repo))).execute($tx)?
    };
    ($tx:expr, $t:ident, $repo:expr, $col:ident = $val:expr) => {
        diesel::delete($t::table.filter($t::repo.eq($repo).and($t::$col.eq($val)))).execute($tx)?
    };
}

/// Multi-row `INSERT` of insertable rows, in batches under the bind-parameter limit.
macro_rules! insert_rows {
    ($tx:expr, $t:ident, $rows:expr) => {
        for batch in $rows.chunks($crate::store::pg::INSERT_ROWS) {
            diesel::insert_into($t::table).values(batch).execute($tx)?;
        }
    };
    ($tx:expr, $t:ident, $rows:expr, do_nothing) => {
        for batch in $rows.chunks($crate::store::pg::INSERT_ROWS) {
            diesel::insert_into($t::table)
                .values(batch)
                .on_conflict_do_nothing()
                .execute($tx)?;
        }
    };
}

/// The repository's rows of a table: the selected columns, or one column by key.
macro_rules! rows {
    ($pg:expr, $t:ident, $sel:expr) => {
        $pg.run(|c| $t::table.filter($t::repo.eq($pg.repo)).select($sel).load(c))
    };
    ($pg:expr, $t:ident, $sel:expr, order $o:ident) => {
        $pg.run(|c| {
            $t::table
                .filter($t::repo.eq($pg.repo))
                .order($t::$o)
                .select($sel)
                .load(c)
        })
    };
}

macro_rules! lookup {
    ($pg:expr, $t:ident, $sel:ident, $col:ident = $key:expr) => {
        $pg.run(|c| {
            $t::table
                .filter($t::repo.eq($pg.repo).and($t::$col.eq($key)))
                .select($t::$sel)
                .first(c)
                .optional()
        })
    };
}

pub(super) use {insert_rows, lookup, remove, rows, upsert};
