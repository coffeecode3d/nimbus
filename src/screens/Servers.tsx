import { useEffect, useMemo, useRef, useState } from "react";
import type { Engine } from "../engine";
import { Button, Cc, LoadBar } from "../components";
import { Search, Star, X } from "../icons";

const fmtUsers = (n: number) => (n >= 1000 ? `${(n / 1000).toFixed(1)}k` : String(n));

type Filter = "all" | "favorites" | "recent";
type Sort = "latency" | "load" | "name";

export function ServersScreen({
  engine,
  notify,
}: {
  engine: Engine;
  notify: (msg: string) => void;
}) {
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const [sort, setSort] = useState<Sort>("latency");
  const [browseId, setBrowseId] = useState(engine.serverId);
  const input = useRef<HTMLInputElement>(null);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        input.current?.focus();
      }
      if (e.key === "Escape") {
        setQuery("");
        input.current?.blur();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const latencyOf = engine.latencyFor;

  const fastest = useMemo(() => {
    return [...engine.servers]
      .filter((s) => latencyOf(s) != null)
      .sort((a, b) => (latencyOf(a) ?? 0) - (latencyOf(b) ?? 0))[0]?.id;
  }, [engine.servers, latencyOf]);

  const countryCount = useMemo(
    () => new Set(engine.servers.map((s) => s.country)).size,
    [engine.servers],
  );

  const groups = useMemo(() => {
    const q = query.trim().toLowerCase();
    let list = engine.servers.filter((s) => {
      if (q && ![s.name, s.city, s.country, s.cc].some((v) => v.toLowerCase().includes(q))) {
        return false;
      }
      if (filter === "favorites") return engine.favorites.includes(s.id);
      if (filter === "recent") return engine.recent.includes(s.id);
      return true;
    });

    list = [...list].sort((a, b) => {
      if (sort === "load") return a.load - b.load;
      if (sort === "name") return a.name.localeCompare(b.name);
      return (latencyOf(a) ?? 9999) - (latencyOf(b) ?? 9999);
    });

    const map = new Map<string, typeof list>();
    for (const s of list) {
      const key = sort === "name" ? s.country : s.continent;
      const arr = map.get(key) ?? [];
      arr.push(s);
      map.set(key, arr);
    }
    return [...map.entries()];
  }, [engine.servers, engine.favorites, engine.recent, query, filter, sort, latencyOf]);

  const selected = engine.servers.find((s) => s.id === browseId) ?? engine.servers[0];
  const isCurrent = selected.id === engine.serverId;
  const favorite = engine.favorites.includes(selected.id);

  const primary = () => {
    if (isCurrent && engine.connected) {
      engine.disconnect();
      return;
    }
    if (!isCurrent) engine.selectServer(selected.id);
    engine.connect();
    notify(`Connecting to ${selected.name}`);
  };

  const total = groups.reduce((n, [, arr]) => n + arr.length, 0);

  return (
    <div className="servers">
      <header className="page-head">
        <div>
          <h1>Servers</h1>
          <p>
            {total} of {engine.servers.length} servers · {countryCount} countries · live
            AirVPN status
          </p>
        </div>
        <div className="search">
          <Search size={17} />
          <input
            ref={input}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search name, city or country"
            aria-label="Search servers"
          />
          {query && (
            <button type="button" className="search-clear" onClick={() => setQuery("")} aria-label="Clear search">
              <X size={14} />
            </button>
          )}
          <kbd>{/Mac/i.test(navigator.userAgent) ? "⌘K" : "Ctrl K"}</kbd>
        </div>
      </header>

      <div className="filters">
        {(["all", "favorites", "recent"] as Filter[]).map((f) => (
          <button
            key={f}
            type="button"
            className={`chip${filter === f ? " is-active" : ""}`}
            onClick={() => setFilter(f)}
          >
            {f === "all" ? "All locations" : f === "favorites" ? "Favorites" : "Recent"}
          </button>
        ))}
        <div className="filters-spacer" />
        {engine.mode === "live" && (
          <button
            type="button"
            className="chip"
            onClick={() => void engine.refreshLatency()}
            disabled={engine.measuringLatency}
            title="Measure TCP round-trip time to every entry IP"
          >
            {engine.measuringLatency ? "Measuring…" : "Measure latency"}
          </button>
        )}
        <label className="sort">
          Sort
          <select value={sort} onChange={(e) => setSort(e.target.value as Sort)}>
            <option value="latency">Latency</option>
            <option value="load">Load</option>
            <option value="name">Country</option>
          </select>
        </label>
      </div>

      <div className="servers-body">
        <div className="list" role="list">
          {groups.length === 0 && (
            <div className="empty">
              <Search size={26} />
              <p>No locations match “{query}”.</p>
            </div>
          )}
          {groups.map(([group, items]) => (
            <div className="group" key={group}>
              <div className="group-head">
                <span>{group}</span>
                <span className="group-count">{items.length}</span>
              </div>
              {items.map((s) => {
                const active = s.id === browseId;
                const live = s.id === engine.serverId && engine.connected;
                const fav = engine.favorites.includes(s.id);
                return (
                  <button
                    type="button"
                    role="listitem"
                    key={s.id}
                    className={`row${active ? " is-active" : ""}`}
                    onClick={() => setBrowseId(s.id)}
                    onDoubleClick={() => {
                      setBrowseId(s.id);
                      engine.selectServer(s.id);
                      engine.connect();
                    }}
                  >
                    <span className={`cc${live ? " is-live" : ""}`}>{s.cc}</span>
                    <span className="row-main">
                      <span className="row-name">
                        {s.name}
                        {s.id === fastest && <span className="tag">Fastest</span>}
                      </span>
                      <span className="row-sub">{s.city}</span>
                    </span>
                    <span className="row-load">
                      <LoadBar load={s.load} />
                      <span>{s.load}%</span>
                    </span>
                    <span className="row-latency mono">
                      {latencyOf(s) != null ? `${latencyOf(s)} ms` : "—"}
                    </span>
                    <span
                      className={`row-star${fav ? " is-fav" : ""}`}
                      role="button"
                      tabIndex={0}
                      aria-label={fav ? `Unfavorite ${s.name}` : `Favorite ${s.name}`}
                      onClick={(e) => {
                        e.stopPropagation();
                        engine.toggleFavorite(s.id);
                      }}
                      onKeyDown={(e) => {
                        if (e.key === "Enter" || e.key === " ") {
                          e.preventDefault();
                          e.stopPropagation();
                          engine.toggleFavorite(s.id);
                        }
                      }}
                    >
                      <Star size={16} filled={fav} />
                    </span>
                  </button>
                );
              })}
            </div>
          ))}
        </div>

        <aside className="detail">
          <div className="detail-top">
            <Cc code={selected.cc} />
            <div>
              <h2>{selected.name}</h2>
              <p>
                {selected.city}, {selected.country}
              </p>
            </div>
          </div>

          <div className="detail-grid">
            <div>
              <span>Latency</span>
              <strong>
                {latencyOf(selected) != null ? `${latencyOf(selected)} ms` : "Measuring…"}
              </strong>
            </div>
            <div>
              <span>Load</span>
              <strong>{selected.load}%</strong>
            </div>
            <div>
              <span>Users</span>
              <strong>{fmtUsers(selected.users)}</strong>
            </div>
            <div>
              <span>Capacity</span>
              <strong>
                {selected.bw >= 1000
                  ? `${Number((selected.bw / 1000).toFixed(1))} Gbps`
                  : `${selected.bw} Mbps`}
              </strong>
            </div>
          </div>

          <div className="detail-ip mono">{selected.ip}</div>

          <Button wide variant={isCurrent && engine.connected ? "ghost" : "primary"} onClick={primary}>
            {isCurrent && engine.connected
              ? "Disconnect"
              : isCurrent
                ? "Connect"
                : "Connect here"}
          </Button>

          <button type="button" className="link-btn" onClick={() => engine.toggleFavorite(selected.id)}>
            <Star size={15} filled={favorite} />
            {favorite ? "Remove from favorites" : "Add to favorites"}
          </button>

          <p className="detail-note">
            Load and users update live from the AirVPN status API. Latency is measured
            locally against each entry IP.
          </p>
        </aside>
      </div>
    </div>
  );
}
