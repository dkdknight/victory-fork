import sys,re
out=[];cur=''
for line in open(sys.argv[1],encoding='utf-8',errors='replace'):
    line=line.rstrip('\r\n')
    if re.match(r'^\d{4}-\d\d-\d\dT',line):
        if cur: out.append(cur)
        cur=line
    else:
        cur+=line.strip() if cur.endswith(' ') else ' '+line.strip() if line.strip() else ''
if cur: out.append(cur)
open(sys.argv[2],'w',encoding='utf-8').write('\n'.join(out))
