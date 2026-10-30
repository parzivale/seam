"""Reading rustdoc JSON."""
import json
class Doc:
    def __init__(self, path):
        d = json.load(open(path))
        self.d = d
        self.index = d['index']
        self.paths = d['paths']
    def item(self, id): return self.index.get(str(id))
    def path_of(self, id):
        p = self.paths.get(str(id))
        return '::'.join(p['path']) if p else None
    def find(self, path):
        for id, p in self.paths.items():
            if '::'.join(p['path']) == path and p['kind'] in ('struct','enum','trait','union'):
                return id
    def public_types(self, krate=0):
        out = {}
        for id, p in self.paths.items():
            if p['crate_id'] == krate and p['kind'] in ('struct', 'enum') and id in self.index:
                out['::'.join(p['path'])] = id
        return out
    def impls(self, tid):
        it = self.item(tid)
        kind = next(iter(it['inner']))
        return [self.item(i) for i in it['inner'][kind]['impls']]
    def type_paths(self, ty, acc=None):
        """Every resolved path id in a type."""
        if acc is None: acc = []
        if isinstance(ty, dict):
            for k, v in ty.items():
                if k == 'resolved_path':
                    acc.append(str(v['id']))
                    self.type_paths(v.get('args'), acc)
                else:
                    self.type_paths(v, acc)
        elif isinstance(ty, list):
            for v in ty: self.type_paths(v, acc)
        return acc

def public_paths(doc):
    """id -> public path, by walking the crate's modules from its root."""
    out = {}
    root = doc.d['root']
    def walk(mid, prefix, seen):
        if mid in seen: return
        seen.add(mid)
        m = doc.item(mid)
        for cid in m['inner']['module']['items']:
            c = doc.item(cid)
            if c is None or c.get('visibility') != 'public': continue
            kind = next(iter(c['inner']))
            if kind == 'use':
                u = c['inner']['use']
                if u['id'] is None: continue
                t = doc.item(u['id'])
                if t is None: continue
                if u['is_glob']:
                    if 'module' in t['inner']: walk(str(u['id']), prefix, seen)
                    continue
                name = u['name']
                if 'module' in t['inner']:
                    walk(str(u['id']), prefix + [name], seen)
                else:
                    out.setdefault(str(u['id']), '::'.join(prefix + [name]))
            elif kind == 'module':
                walk(cid if isinstance(cid, str) else str(cid), prefix + [c['name']], seen)
            else:
                out.setdefault(str(cid), '::'.join(prefix + [c['name']]))
    walk(str(root), [doc.item(root)['name']], set())
    return out
