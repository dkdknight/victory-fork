import csv,collections,re,sys
rows=list(csv.reader(open(sys.argv[1],encoding='utf-8-sig'),skipinitialspace=True))
h=[x.strip() for x in rows[0]]; I={n:i for i,n in enumerate(h)}
D=I['EOP to EOP Duration (ns)']
def dur(r):
    try: return float(r[D])
    except: return 0.0
WORK=('Dispatch','DispatchMesh','ExecuteIndirect','DrawInstanced','DrawIndexedInstanced')
agg=collections.defaultdict(lambda:[0,0.0,0.0,''])
cur=None; unnamed=collections.defaultdict(lambda:[0,0.0])
total=0.0
for r in rows[1:]:
    name=r[I['Name']].strip()
    d=dur(r); total+=d
    if name.startswith('<deprecated'):
        t=name.split('> ',1)[1]
        kind=t.split(' ')[0]
        m=re.search(r'\[(?:mesh|compute|vertex|pixel)[^\]]*\]',t)
        cur=(kind+' '+(m.group(0) if m else t[:40]), t)
        continue
    k=re.split(r'[\(\s]',name)[0]
    if k in WORK and cur:
        a=agg[cur[0]]; a[0]+=1; a[1]+=d; a[2]=max(a[2],d); a[3]=cur[1]
        cur=None
    elif d>0:
        u=unnamed[k]; u[0]+=1; u[1]+=d
print('total ms',round(total/1e6,1),'named ms',round(sum(a[1] for a in agg.values())/1e6,1))
print('unnamed:',[(k,v[0],round(v[1]/1e6,2)) for k,v in sorted(unnamed.items(),key=lambda x:-x[1][1])[:8]])
kinds=collections.defaultdict(lambda:[0,0.0])
for k,a in agg.items():
    w=k.split(' ')[0]; kinds[w][0]+=a[0]; kinds[w][1]+=a[1]
print({k:(v[0],round(v[1]/1e6,1)) for k,v in kinds.items()})
for k,a in sorted(agg.items(),key=lambda x:-x[1][1])[:int(sys.argv[2]) if len(sys.argv)>2 else 45]:
    print(f"{a[1]/1e6:7.2f} ms n={a[0]:4d} max={a[2]/1e3:7.0f}us  {k}  || {a[3][len(k.split(' ')[0]):][:110]}")
