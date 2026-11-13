import subprocess, re, sys, collections
import os
T=os.path.dirname(os.path.abspath(__file__))+'/'
REPO=subprocess.check_output(['git','-C',T,'rev-parse','--show-toplevel'],text=True).strip()
cpu={}
for l in open(T+'runs/cold-bc/cpu'):
    c,d=l.split(); cpu[d]=float(c)
graph={}
for l in open(T+'bc-graph'):
    p=l.split(); graph[p[0]]=p[1:]
rev=collections.defaultdict(set)
for d,deps in graph.items():
    for x in deps: rev[x].add(d)
def nv(d):
    m=re.match(r'/nix/store/[a-z0-9]{32}-rust_(.+)\.drv$', d); return m.group(1)
byname=collections.defaultdict(list)
for d in graph:
    s=nv(d)
    # name-version: split at the last '-' followed by a digit
    m=re.match(r'(.+?)-(\d+\.\d+\.\d+.*)$', s); byname[m.group(1)].append(d)
def affected(names):
    seen=set(); stack=[d for n in names for d in byname.get(n,[])]
    while stack:
        d=stack.pop()
        if d in seen: continue
        seen.add(d); stack.extend(rev[d])
    return seen
# workspace dir -> crate name
ws={}
for line in subprocess.check_output(['git','-C',REPO,'ls-tree','-r','--name-only','origin/main','rs'],text=True).split():
    if line.endswith('/Cargo.toml'):
        dirn=line[:-len('/Cargo.toml')]
        txt=subprocess.check_output(['git','-C',REPO,'show','origin/main:'+line],text=True)
        m=re.search(r'^\[package\]\s*\nname\s*=\s*"([^"]+)"',txt,re.M)
        if m: ws[dirn]=m.group(1)
def lockset(rev):
    try: txt=subprocess.check_output(['git','-C',REPO,'show',rev+':Cargo.lock'],text=True,stderr=subprocess.DEVNULL)
    except subprocess.CalledProcessError: return set()
    return set(re.findall(r'name = "([^"]+)"\nversion = "([^"]+)"',txt))
cargoish=lambda f: f.endswith('.rs') or f.endswith('Cargo.toml') or f=='Cargo.lock' or f.endswith('.cargo/config.toml')
commits=[]; cur=None
for l in open(T+'week.txt'):
    l=l.strip()
    if l.startswith('@'): cur=[l[1:],[]]; commits.append(cur)
    elif l: cur[1].append(l)
CRANE_EDIT=776; CRANE_FULL=1484
tot=collections.Counter(); cnt=collections.Counter()
for h,files in commits:
    files=[f for f in files if cargoish(f)]
    if not files: cnt['noop']+=1; continue
    names=set()
    lock='Cargo.lock' in files
    manifest=any(f.endswith('Cargo.toml') for f in files)
    for f in files:
        best=max((d for d in ws if f.startswith(d+'/')), key=len, default=None)
        if best: names.add(ws[best])
    if lock:
        a=lockset(h+'^'); b=lockset(h)
        names|={n for n,v in (a^b)}
    aff=affected(names)
    pc=sum(cpu.get(d,0) for d in aff)
    full=lock or manifest or any(f.startswith('rs/kio/src/') for f in files)
    cr=CRANE_FULL if full else CRANE_EDIT
    kind='lock/manifest' if full else 'source'
    cnt[kind]+=1; tot[kind+' crane']+=cr; tot[kind+' percrate']+=pc
    cnt[kind+' percrate-noop']+= (pc==0)
    tot[kind+' rebuilt']+=len(aff)
print('commits',len(commits),dict(cnt))
for k in sorted(tot): print(k, round(tot[k]))
print('crane total', round(tot['source crane']+tot['lock/manifest crane']), 'per-crate total', round(tot['source percrate']+tot['lock/manifest percrate']))
size={}
for l in open(T+'bc-sizes'):
    d,n,b=l.split(); size[d]=(int(n),int(b))
P=collections.Counter()
for h,files in commits:
    files=[f for f in files if cargoish(f)]
    if not files: continue
    names=set(); lock='Cargo.lock' in files
    for f in files:
        best=max((d for d in ws if f.startswith(d+'/')), key=len, default=None)
        if best: names.add(ws[best])
    if lock:
        a=lockset(h+'^'); b=lockset(h); names|={n for n,v in (a^b)}
    aff=affected(names)
    full=lock or any(f.endswith('Cargo.toml') for f in files) or any(f.startswith('rs/kio/src/') for f in files)
    P['pc paths']+=sum(size[d][0] for d in aff)+(1 if aff else 0); P['pc bytes']+=sum(size[d][1] for d in aff)
    P['crane paths']+= 10 if full else 3; P['crane bytes']+= (574 if full else 27)*1048576
print({k:(v if 'paths' in k else round(v/2**30,1)) for k,v in P.items()}, 'GiB for bytes')
