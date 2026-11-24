import { useCallback, useEffect, useRef, useState } from 'react';
import type { FormEvent, ReactNode } from 'react';
import { ApiError, canManageUsers, canWrite, componentPath, errorMessage, importBody, json, listPath, request } from './api';
import type { Component, Fields, Filters, Relation, User } from './api';
import Teams from './Teams';

type View = { kind: 'catalog' | 'teams' } | { kind: 'detail' | 'edit'; namespace: string; name: string } | { kind: 'create' | 'import' };
const PAGE_SIZE = 20;
const initialFilters: Filters = { q: '', namespace: '' };

function ErrorNotice({ message }: { message: string }) {
  return message ? <p className="error" role="alert">{message}</p> : null;
}
function Heading({ title, children }: { title: string; children?: ReactNode }) {
  const ref = useRef<HTMLHeadingElement>(null);
  useEffect(() => { ref.current?.focus(); }, [title]);
  return <div className="heading"><div><h1 ref={ref} tabIndex={-1}>{title}</h1>{children}</div></div>;
}
function Login({ onLogin }: { onLogin: (user: User) => void }) {
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault(); setBusy(true); setError('');
    const fields = new FormData(event.currentTarget);
    try { onLogin(await request<User>('/api/auth/login', json('POST', { username: fields.get('username'), password: fields.get('password') }))); }
    catch (e) { setError(errorMessage(e)); }
    finally { setBusy(false); }
  }
  return <main className="login" id="main"><div className="login-panel"><div className="wordmark">Panggung<span>Developer portal</span></div><Heading title="Sign in"><p>Access your engineering catalog.</p></Heading><form onSubmit={submit} className="form">
    <label>Username<input name="username" autoComplete="username" required maxLength={100} /></label>
    <label>Password<input name="password" type="password" autoComplete="current-password" required /></label>
    <ErrorNotice message={error} /><button className="primary" disabled={busy}>{busy ? 'Signing in…' : 'Sign in'}</button>
  </form></div></main>;
}

function Catalog({ write, navigate, run, filters, setFilters, offset, setOffset }: {
  write: boolean; navigate: (view: View) => void; run: typeof request;
  filters: Filters; setFilters: (filters: Filters) => void; offset: number; setOffset: (offset: number) => void;
}) {
  const [draft, setDraft] = useState(filters);
  const [items, setItems] = useState<Component[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [retry, setRetry] = useState(0);
  useEffect(() => {
    const controller = new AbortController();
    setLoading(true); setError('');
    run<Component[]>(listPath(filters, offset, PAGE_SIZE + 1), { signal: controller.signal })
      .then(data => { if (!controller.signal.aborted) setItems(data); })
      .catch(e => { if (!controller.signal.aborted) setError(errorMessage(e)); })
      .finally(() => { if (!controller.signal.aborted) setLoading(false); });
    return () => controller.abort();
  }, [filters, offset, retry, run]);
  function search(event: FormEvent) { event.preventDefault(); setOffset(0); setFilters({ q: draft.q.trim(), namespace: draft.namespace.trim() }); }
  const visible = items.slice(0, PAGE_SIZE);
  return <>
    <div className="page-top"><Heading title="Software catalog"><p>Discover components, ownership and declared dependencies.</p></Heading>
      {write && <div className="actions"><button onClick={() => navigate({kind:'import'})}>Import descriptor</button><button className="primary" onClick={() => navigate({kind:'create'})}>Register component</button></div>}
    </div>
    <section className="panel" aria-label="Components">
      <form className="filters" onSubmit={search}>
        <label>Search components<input type="search" value={draft.q} maxLength={200} placeholder="Name, description or owner" onChange={e => setDraft({...draft, q:e.target.value})}/></label>
        <label>Namespace<input value={draft.namespace} maxLength={63} placeholder="All namespaces" onChange={e => setDraft({...draft, namespace:e.target.value})}/></label>
        <button className="primary">Search</button><button type="button" onClick={() => {setDraft(initialFilters);setOffset(0);setFilters({...initialFilters});}}>Clear</button>
      </form>
      <div className="table-title"><h2>Components</h2><span role="status">{loading ? 'Loading components…' : error ? 'Catalog unavailable' : visible.length ? 'Showing ' + (offset + 1) + '–' + (offset + visible.length) : 'No components on this page'}</span></div>
      {error ? <div className="empty"><ErrorNotice message={error}/><button onClick={() => setRetry(n => n+1)}>Try again</button></div> : loading ? <p className="empty" role="status">Retrieving catalog records…</p> : !visible.length ? <div className="empty"><h3>{filters.q || filters.namespace ? 'No matching components' : offset ? 'No more components' : 'No components registered'}</h3><p>{filters.q || filters.namespace ? 'Try a different search or clear the filters.' : offset ? 'Go back to the previous page.' : write ? 'Register a component or import a descriptor to get started.' : 'Ask a catalog editor to register components.'}</p></div> :
        <div className="table-scroll" tabIndex={0} role="region" aria-label="Component table"><table><thead><tr><th scope="col">Component</th><th scope="col">Namespace</th><th scope="col">Owner</th><th scope="col">Type</th><th scope="col">Lifecycle</th></tr></thead><tbody>{visible.map(item => <tr key={item.namespace+'/'+item.name}><td><button className="text-link" onClick={() => navigate({kind:'detail',namespace:item.namespace,name:item.name})}>{item.name}</button>{item.description && <small>{item.description}</small>}</td><td><code>{item.namespace}</code></td><td>{item.owner}</td><td>{item.component_type ?? '—'}</td><td>{item.lifecycle ?? '—'}</td></tr>)}</tbody></table></div>}
      <div className="pagination"><span>Page {Math.floor(offset/PAGE_SIZE)+1}</span><div className="actions"><button disabled={loading || offset===0} onClick={() => setOffset(Math.max(0,offset-PAGE_SIZE))}>Previous</button><button disabled={loading || !!error || items.length<=PAGE_SIZE} onClick={() => setOffset(offset+PAGE_SIZE)}>Next</button></div></div>
    </section>
  </>;
}

function ComponentForm({ item, run, onSaved, cancel }: { item?: Component; run: typeof request; onSaved:(item:Component)=>void; cancel:()=>void }) {
  const [busy,setBusy]=useState(false);
  const [error,setError]=useState('');
  async function submit(event:FormEvent<HTMLFormElement>) {
    event.preventDefault(); setBusy(true);setError('');
    const data=new FormData(event.currentTarget);
    const fields:Fields={name:String(data.get('name')),owner:String(data.get('owner')),description:String(data.get('description'))};
    try {onSaved(await run<Component>(item ? componentPath(item.namespace,item.name) : '/api/components',json(item?'PUT':'POST',fields)));}
    catch(e){setError(errorMessage(e));}finally{setBusy(false);}
  }
  return <form className="panel form editor" onSubmit={submit}>
    <label>Name<input name="name" required maxLength={63} pattern="[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?" defaultValue={item?.name} readOnly={!!item}/></label>
    <label>Namespace<input readOnly value={item?.namespace ?? 'default'}/></label>
    <label>Owner<input name="owner" required maxLength={100} defaultValue={item?.owner} /></label>
    <label>Description<textarea name="description" maxLength={2000} rows={5} defaultValue={item?.description}/></label>
    <ErrorNotice message={error}/><div className="actions"><button type="button" onClick={cancel} disabled={busy}>Cancel</button><button className="primary" disabled={busy}>{busy?'Saving…':'Save component'}</button></div>
  </form>;
}
function ImportForm({run,onSaved,cancel}:{run:typeof request;onSaved:(item:Component)=>void;cancel:()=>void}) {
  const [format,setFormat]=useState<'json'|'yaml'>('yaml');
  const [value,setValue]=useState('');
  const [busy,setBusy]=useState(false);
  const [error,setError]=useState('');
  async function submit(e:FormEvent) {
    e.preventDefault();setBusy(true);setError('');
    try { const {path,init}=importBody(format,value);onSaved(await run<Component>(path,init)); }
    catch(err){setError(errorMessage(err));}finally{setBusy(false);}
  }
  return <><Heading title="Import component"><p>Import one Backstage Component v1alpha1 descriptor. Remote references are not fetched.</p></Heading>
    <form className="panel form editor" onSubmit={submit}>
      <label>Descriptor format<select value={format} onChange={e=>setFormat(e.target.value as 'json'|'yaml')}><option value="yaml">YAML</option><option value="json">JSON</option></select></label>
      <label>Descriptor<textarea className="code-input" required rows={16} spellCheck={false} value={value} onChange={e=>setValue(e.target.value)} placeholder={format==='yaml'?'apiVersion: backstage.io/v1alpha1\nkind: Component':'{"apiVersion":"backstage.io/v1alpha1","kind":"Component",…}'}/></label>
      <p className="muted">Maximum 64 KiB. Component name, type, lifecycle and owner are required.</p><ErrorNotice message={error}/>
      <div className="actions"><button type="button" onClick={cancel} disabled={busy}>Cancel</button><button className="primary" disabled={busy}>{busy?'Importing…':'Import component'}</button></div>
    </form></>;
}
function Detail({namespace,name,editing,write,run,navigate,onSaved,onDeleted}:{namespace:string;name:string;editing:boolean;write:boolean;run:typeof request;navigate:(v:View)=>void;onSaved:(c:Component)=>void;onDeleted:()=>void}) {
  const [item,setItem]=useState<Component|null>(null);
  const [relations,setRelations]=useState<Relation[]|null>(null);
  const [relationError,setRelationError]=useState('');
  const [error,setError]=useState('');
  const [retry,setRetry]=useState(0);
  const [deleting,setDeleting]=useState(false);
  const [busy,setBusy]=useState(false);
  const dialog=useRef<HTMLDialogElement>(null);
  const deleteButton=useRef<HTMLButtonElement>(null);
  useEffect(()=>{
    const controller=new AbortController();setItem(null);setRelations(null);setError('');setRelationError('');
    run<Component>(componentPath(namespace,name),{signal:controller.signal})
      .then(value=>{if(!controller.signal.aborted)setItem(value);})
      .catch(e=>{if(!controller.signal.aborted)setError(errorMessage(e));});
    run<Relation[]>('/api/catalog/relations/'+encodeURIComponent(namespace)+'/'+encodeURIComponent(name),{signal:controller.signal})
      .then(value=>{if(!controller.signal.aborted)setRelations(value);})
      .catch(e=>{if(!controller.signal.aborted)setRelationError(errorMessage(e));});
    return ()=>controller.abort();
  },[namespace,name,retry,run]);
  async function remove(){
    setBusy(true);setError('');
    try {await run<void>(componentPath(namespace,name),{method:'DELETE'});dialog.current?.close();onDeleted();}
    catch(e){setError(errorMessage(e));dialog.current?.close();}
    finally{setBusy(false);}
  }
  const back=()=>navigate({kind:'catalog'});
  if(!item) return <><button className="text-link" onClick={back}>← Catalog</button><Heading title={name}/>{error?<><ErrorNotice message={error}/><button onClick={()=>setRetry(n=>n+1)}>Try again</button></>:<p role="status">Loading component…</p>}</>;
  if(editing && write) return <><Heading title={'Edit '+item.name}/><ComponentForm item={item} run={run} onSaved={onSaved} cancel={()=>navigate({kind:'detail',namespace,name})}/></>;
  return <><button className="text-link" onClick={back}>← Catalog</button>
    <div className="page-top"><Heading title={item.name}><p><code>{item.namespace}/{item.name}</code></p></Heading>{write&&<div className="actions"><button onClick={()=>navigate({kind:'edit',namespace,name})}>Edit component</button><button ref={deleteButton} className="danger" onClick={()=>{setDeleting(true);dialog.current?.showModal();}}>Delete</button></div>}</div>
    <ErrorNotice message={error}/>
    <section className="panel section"><h2>Overview</h2><p>{item.description||'No description provided.'}</p><dl className="facts"><div><dt>Owner</dt><dd>{item.owner}</dd></div><div><dt>Type</dt><dd>{item.component_type??'Not specified'}</dd></div><div><dt>Lifecycle</dt><dd>{item.lifecycle??'Not specified'}</dd></div></dl></section>
    <section className="panel section"><h2>Declared relationships</h2><p className="muted">References are declarations; target existence has not been verified.</p>{relationError?<ErrorNotice message={relationError}/>:relations===null?<p role="status">Loading relationships…</p>:relations.length===0?<p>No relationships declared.</p>:<ul className="relations">{relations.map((r,i)=><li key={r.type+r.targetRef+i}><span>{r.type}</span><code>{r.targetRef}</code></li>)}</ul>}</section>
    <section className="panel section"><h2>Stored descriptor</h2>{item.descriptor?<pre>{JSON.stringify(item.descriptor,null,2)}</pre>:<p>Registered manually. No descriptor stored.</p>}</section>
    <dialog ref={dialog} aria-labelledby="delete-title" onClose={()=>{setDeleting(false);deleteButton.current?.focus();}} onCancel={e=>{if(busy)e.preventDefault();}}>
      <h2 id="delete-title">Delete component?</h2><p>Remove <strong>{namespace}/{name}</strong> from the catalog. This cannot be undone.</p><div className="actions"><button autoFocus disabled={busy} onClick={()=>dialog.current?.close()}>Cancel</button><button className="danger" disabled={busy||!deleting} onClick={remove}>{busy?'Deleting…':'Delete component'}</button></div>
    </dialog>
  </>;
}
export default function App(){
  const [user,setUser]=useState<User|null>(null);
  const [sessionLoading,setSessionLoading]=useState(true);
  const [sessionError,setSessionError]=useState('');
  const [sessionRetry,setSessionRetry]=useState(0);
  const [view,setView]=useState<View>({kind:'catalog'});
  const [filters,setFilters]=useState<Filters>(initialFilters);
  const [offset,setOffset]=useState(0);
  const [notice,setNotice]=useState('');
  const [logoutBusy,setLogoutBusy]=useState(false);
  useEffect(()=>{
    const controller=new AbortController();setSessionLoading(true);setSessionError('');
    request<User>('/api/auth/me',{signal:controller.signal}).then(value=>{if(!controller.signal.aborted)setUser(value);}).catch(e=>{if(!controller.signal.aborted){if(e instanceof ApiError&&e.status===401)setUser(null);else setSessionError(errorMessage(e));}}).finally(()=>{if(!controller.signal.aborted)setSessionLoading(false);});
    return ()=>controller.abort();
  },[sessionRetry]);
  const run=useCallback(async <T,>(path:string,init?:RequestInit):Promise<T>=>{
    try{return await request<T>(path,init);}catch(e){if(e instanceof ApiError&&e.status===401){setUser(null);setView({kind:'catalog'});setNotice('');}throw e;}
  },[]);
  function navigate(v:View){setNotice('');setView(v);}
  function onSaved(c:Component){setNotice('Component saved.');setView({kind:'detail',namespace:c.namespace,name:c.name});}
  async function logout(){setLogoutBusy(true);setSessionError('');try{await run<void>('/api/auth/logout',{method:'POST'});setUser(null);setView({kind:'catalog'});setFilters(initialFilters);setOffset(0);setNotice('');}catch(e){setSessionError(errorMessage(e));}finally{setLogoutBusy(false);}}
  if(sessionLoading)return <main className="login" role="status">Checking your session…</main>;
  if(!user&&sessionError)return <main className="login"><div className="login-panel"><Heading title="Session unavailable"/><ErrorNotice message={sessionError}/><button onClick={()=>setSessionRetry(n=>n+1)}>Try again</button></div></main>;
  if(!user)return <Login onLogin={value=>{setSessionError('');setUser(value);setView({kind:'catalog'});}}/>;
  const write=canWrite(user);
  return <><a className="skip" href="#main">Skip to content</a><header className="topbar"><div className="brand">Panggung<span>Developer portal</span></div><nav aria-label="Main navigation"><button className="nav-link" aria-current={view.kind!=='teams'?'page':undefined} onClick={()=>navigate({kind:'catalog'})}>Catalog</button><button className="nav-link" aria-current={view.kind==='teams'?'page':undefined} onClick={()=>navigate({kind:'teams'})}>Teams</button></nav><div className="account"><div><strong>{user.username}</strong><small>{user.role}</small></div><button disabled={logoutBusy} onClick={logout}>{logoutBusy?'Signing out…':'Sign out'}</button></div></header>
    <main id="main" className="workspace"><ErrorNotice message={sessionError}/>{notice&&<p className="notice" role="status">{notice}</p>}
      {view.kind==='catalog'?<Catalog write={write} navigate={navigate} run={run} filters={filters} setFilters={setFilters} offset={offset} setOffset={setOffset}/>:
      view.kind==='teams'?<Teams run={run} admin={canManageUsers(user)}/>:
      view.kind==='create'&&write?<><Heading title="Register component"><p>Register a component in the default namespace.</p></Heading><ComponentForm run={run} onSaved={onSaved} cancel={()=>navigate({kind:'catalog'})}/></>:
      view.kind==='import'&&write?<ImportForm run={run} onSaved={onSaved} cancel={()=>navigate({kind:'catalog'})}/>:
      view.kind==='detail'||view.kind==='edit'?<Detail key={view.kind+'/'+view.namespace+'/'+view.name} namespace={view.namespace} name={view.name} editing={view.kind==='edit'} write={write} run={run} navigate={navigate} onSaved={onSaved} onDeleted={()=>{setView({kind:'catalog'});setNotice('Component deleted.');}}/>:<p role="alert">You do not have permission to access this page.</p>}
    </main><footer> Panggung · Software catalog </footer></>;
}
