import sys; sys.path.insert(0,'.')
from rd import Doc, public_paths
EXCLUDED_TYPES = {'jiff::tz::TimeZoneDatabase','jiff::tz::TimeZoneName','jiff::tz::TimeZoneNameIter'}
SEEDS = ['jiff::Timestamp','jiff::Zoned','jiff::civil::Date','jiff::civil::DateTime','jiff::tz::TimeZone','jiff::Span']
def self_id(ty):
    if 'borrowed_ref' in ty: ty = ty['borrowed_ref']['type']
    return str(ty['resolved_path']['id']) if 'resolved_path' in ty else None
def fields_of(doc, it):
    inner = it['inner']
    if 'struct' in inner:
        k = inner['struct']['kind']
        if 'plain' in k: return [str(f) for f in k['plain']['fields']]
        if 'tuple' in k: return [str(f) for f in k['tuple'] if f is not None]
        return []
    if 'enum' in inner:
        out = []
        for v in inner['enum']['variants']:
            k = doc.item(v)['inner']['variant']['kind']
            if 'struct' in k: out += [str(f) for f in k['struct']['fields']]
            elif 'tuple' in k: out += [str(f) for f in k['tuple'] if f is not None]
        return out
    return []
def compute(doc, pp, excluded_fn):
    by_path = {v:k for k,v in pp.items()}
    types = {p:i for p,i in by_path.items() if next(iter(doc.item(i)['inner'])) in ('struct','enum') and p not in EXCLUDED_TYPES}
    wrapped = {by_path[s] for s in SEEDS}
    reasons = {}
    def mentions(x):
        return [p for p in doc.type_paths(x) if p in wrapped]
    changed = True
    while changed:
        changed = False
        for path, tid in types.items():
            if tid in wrapped: continue
            why = None
            it = doc.item(tid); kind = next(iter(it['inner']))
            # fields / variants
            if any(mentions(doc.item(f)['inner']) for f in fields_of(doc, it)): why = 'fields'
            for imp in doc.impls(tid):
                i = imp['inner']['impl']
                if i.get('blanket_impl') or i.get('is_synthetic'): continue
                if self_id(i['for']) != tid: continue
                if i['trait'] and i['trait']['path'] in ('Custom',): continue
                tr = i['trait']['path'] if i['trait'] else None
                if tr is None:
                    for fid in i['items']:
                        f = doc.item(fid)
                        if f['visibility'] != 'public' or 'function' not in f['inner']: continue
                        if excluded_fn(path, f['name']): continue
                        if mentions(f['inner']['function']['sig']): why = why or f"method {f['name']}"
                else:
                    if tr in ('From', 'TryFrom'):
                        a = i['trait']['args']['angle_bracketed']['args'][0]['type']
                        direct = ('resolved_path' in a and str(a['resolved_path']['id']) in wrapped) or \
                                 ('borrowed_ref' in a and 'resolved_path' in a['borrowed_ref']['type'] and str(a['borrowed_ref']['type']['resolved_path']['id']) in wrapped)
                        if mentions(a) and not direct: why = why or f"{tr} tuple"
                        continue
                    allm = mentions(i['trait']) + [p for fid in i['items'] for p in mentions(doc.item(fid)['inner'])]
                    if allm: why = why or f"impl {tr}"
            if why:
                wrapped.add(tid); reasons[path] = why; changed = True
    return wrapped, reasons, types
if __name__ == '__main__':
    doc = Doc(sys.argv[1]); pp = public_paths(doc)
    EXCL = {('jiff::Timestamp','now'),('jiff::Zoned','now'),('jiff::tz::TimeZone','system'),('jiff::tz::TimeZone','try_system'),('jiff::tz::TimeZone','get')}
    def excluded_fn(t, n): return (t, n) in EXCL or n in ('in_tz', 'intz') or n.endswith('_with') and n in ('parse_zoned_with','parse_time_zone_with','to_zoned_with','to_time_zone_with')
    w, r, types = compute(doc, pp, excluded_fn)
    for p in sorted(types):
        if types[p] in w: print('WRAP', p, '-', r.get(p, 'seed'))
    print()
    for p in sorted(types):
        if types[p] not in w: print('keep', p)
