import { useEffect, useRef, useState } from 'react';
import type { FormEvent } from 'react';
import { errorMessage, entityKey, structureRequest } from './api';
import type { Domain, System, request } from './api';
import './structure.css';

function EntityForm({ kind, domains, run, onSaved, disabled }: {
  kind: 'domains' | 'systems'; domains: Domain[]; run: typeof request;
  onSaved: () => void; disabled: boolean;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [domain, setDomain] = useState('');
  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (busy || disabled) return;
    const form = event.currentTarget;
    const fields = new FormData(form);
    setBusy(true); setError('');
    try {
      const selected = domain ? domains.find(item => entityKey(item) === domain) : undefined;
      if (domain && !selected) throw new Error('Select an available domain.');
      const { path, init } = structureRequest(kind, String(fields.get('namespace')), String(fields.get('name')), selected);
      await run<Domain | System>(path, init);
      form.reset(); setDomain(''); onSaved();
    } catch (e) { setError(errorMessage(e)); }
    finally { setBusy(false); }
  }
  const label = kind === 'domains' ? 'domain' : 'system';
  return <form className="form structure-form" onSubmit={submit}>
    <h3>Register {label}</h3>
    <p className="muted">Use 1–63 lowercase letters, digits, dots, underscores or hyphens. Start and end with a letter or digit.</p>
    <fieldset disabled={busy || disabled}>
      <label>{label === 'domain' ? 'Domain' : 'System'} namespace<input name="namespace" required defaultValue="default" maxLength={63}/></label>
      <label>{label === 'domain' ? 'Domain' : 'System'} name<input name="name" required maxLength={63}/></label>
      {kind === 'systems' && <label>Parent domain (optional)<select value={domain} onChange={e=>setDomain(e.target.value)}>
        <option value="">No domain</option>
        {domains.map(item=><option key={entityKey(item)} value={entityKey(item)}>{entityKey(item)}</option>)}
      </select></label>}
      <button className="primary" disabled={busy || disabled}>{busy ? 'Registering…' : 'Register '+label}</button>
    </fieldset>
    {error && <p className="error" role="alert">{error}</p>}
  </form>;
}

export default function Structure({ run, write }: { run: typeof request; write: boolean }) {
  const heading = useRef<HTMLHeadingElement>(null);
  const [domains, setDomains] = useState<Domain[]>([]);
  const [systems, setSystems] = useState<System[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [refresh, setRefresh] = useState(0);
  useEffect(()=>{heading.current?.focus();},[]);
  useEffect(()=>{
    const controller = new AbortController();
    setLoading(true); setError('');
    Promise.all([
      run<Domain[]>('/api/catalog/domains',{signal:controller.signal}),
      run<System[]>('/api/catalog/systems',{signal:controller.signal}),
    ]).then(([domainList,systemList])=>{
      if (!controller.signal.aborted) { setDomains(domainList); setSystems(systemList); }
    }).catch(e=>{if (!controller.signal.aborted) setError(errorMessage(e));})
      .finally(()=>{if (!controller.signal.aborted) setLoading(false);});
    return ()=>controller.abort();
  },[run,refresh]);
  function saved(kind: string) { setNotice(kind+' registered.'); setRefresh(n=>n+1); }
  return <>
    <div className="heading"><h1 ref={heading} tabIndex={-1}>Catalog structure</h1><p>Organize engineering systems into business domains. References use qualified namespace and name.</p></div>
    {!write && <p className="muted">You have read-only access to catalog structure.</p>}
    {notice && <p role="status" className="notice">{notice}</p>}
    {error ? <div className="panel section"><p className="error" role="alert">{error}</p><button onClick={()=>setRefresh(n=>n+1)}>Try again</button></div> : loading ? <p role="status">Loading domains and systems…</p> : null}
    <div className="structure-grid">
      <section className="panel section">
        <h2>Domains</h2><p className="muted">Business areas that group related systems.</p>
        {!loading && !error && (domains.length ? <ul className="structure-list">{domains.map(item=><li key={entityKey(item)}><strong>{item.name}</strong><code>{item.namespace}</code></li>)}</ul> : <p>No domains registered.</p>)}
        {!loading && !error && domains.length===500 && <p className="muted">Showing the first 500 domains.</p>}
        {write && <EntityForm kind="domains" domains={domains} run={run} disabled={loading || !!error} onSaved={()=>saved('Domain')}/>}
      </section>
      <section className="panel section">
        <h2>Systems</h2><p className="muted">Collections of components, optionally associated with a domain.</p>
        {!loading && !error && (systems.length ? <ul className="structure-list">{systems.map(item=><li key={entityKey(item)}><strong>{item.name}</strong><code>{item.namespace}</code><small>{item.domain_namespace && item.domain_name ? 'Domain: '+item.domain_namespace+'/'+item.domain_name : 'No domain assigned'}</small></li>)}</ul> : <p>No systems registered.</p>)}
        {!loading && !error && systems.length===500 && <p className="muted">Showing the first 500 systems.</p>}
        {write && <EntityForm kind="systems" domains={domains} run={run} disabled={loading || !!error} onSaved={()=>saved('System')}/>}
      </section>
    </div>
  </>;
}
