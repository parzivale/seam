"""Generates seam-jiff's facade over jiff from jiff's rustdoc JSON.

Every jiff type whose API reaches a type with effectful functions (the
clock, the system time zone, the global tz database) is wrapped: structs
in newtypes, enums mirrored, with every pure method and trait impl
forwarded and every wrapped type in a signature replaced by its wrapper.
The effectful functions are left out; seam-jiff writes their replacements
by hand, over effects. Every other jiff item is re-exported as it is.
"""
import json
import sys

sys.path.insert(0, __file__.rsplit('/', 1)[0])
from rd import Doc, public_paths  # noqa: E402
from closure import compute, fields_of, self_id  # noqa: E402

# Effectful, by (type, method): left out.
EXCLUDED_FNS = {
    ('jiff::Timestamp', 'now'),
    ('jiff::Zoned', 'now'),
    ('jiff::tz::TimeZone', 'system'),
    ('jiff::tz::TimeZone', 'try_system'),
    ('jiff::tz::TimeZone', 'get'),
}
# Effectful wherever they are.
EXCLUDED_NAMES = {
    'in_tz', 'intz',
    'parse_zoned_with', 'parse_time_zone_with', 'to_zoned_with', 'to_time_zone_with',
}
# Effectful because they use the global tz database: (type, method).
DB_FNS = {
    ('jiff::fmt::temporal::DateTimeParser', 'parse_zoned'),
    ('jiff::fmt::temporal::DateTimeParser', 'parse_time_zone'),
    ('jiff::fmt::strtime::BrokenDownTime', 'to_zoned'),
    ('jiff::fmt::temporal::TimeZoneAnnotation', 'to_time_zone'),
    ('jiff::fmt::temporal::TimeZoneAnnotationKind', 'to_time_zone'),
}
# Trait impls left out: (type, trait, first trait arg or None).
EXCLUDED_IMPLS = {
    ('jiff::Zoned', 'FromStr'),  # resolves IANA names in the global database
    ('jiff::Zoned', 'TryFrom'),  # from SystemTime: the system's time zone
}
SKIP_TRAITS = {'Custom', 'StructuralPartialEq'}
# Wrapped, but written by hand in seam-jiff (re-exported where it belongs).
MANUAL = {'jiff::fmt::temporal::DateTimeParser': 'crate::effectful::DateTimeParser'}
EXCLUDED_TYPES = {'jiff::tz::TimeZoneDatabase', 'jiff::tz::TimeZoneName', 'jiff::tz::TimeZoneNameIter'}
EXCLUDED_FREE = {'jiff::tz::db'}


def excluded_fn(tpath, name):
    return (tpath, name) in EXCLUDED_FNS or name in EXCLUDED_NAMES or (tpath, name) in DB_FNS


class Gen:
    def __init__(self, doc):
        self.doc = doc
        self.pp = public_paths(doc)
        self.by_path = {v: k for k, v in self.pp.items()}
        self.wrapped, _, self.types = compute(doc, self.pp, excluded_fn)
        self.enums = {i for i in self.wrapped if 'enum' in doc.item(i)['inner']}
        self.out = {}  # module -> [code]

    # ---- names

    def jiff_path(self, id):
        id = str(id)
        if id in self.pp:
            return self.pp[id]
        p = self.doc.paths.get(id)
        if p is None:
            raise KeyError(id)
        path = p['path']
        if path[0] == 'jiff':
            raise KeyError(f'no public path for {path}')
        if path[0] in ('core', 'alloc', 'std'):
            # Defining paths go through private modules: the public one.
            private = {'traits', 'iterator', 'arith', 'bit', 'range', 'function', 'double_ended',
                       'exact_size', 'accum', 'collect', 'unary', 'deref', 'index', 'try_trait'}
            path = ['std'] + [x for x in path[1:] if x not in private]
            if path[1:3] == ['iter', 'marker']:
                path = ['std', 'iter', path[-1]]
        return '::'.join(path)

    def our_path(self, id):
        jp = self.jiff_path(id)
        assert jp.startswith('jiff::'), jp
        return 'crate::jiff_facade::' + jp[len('jiff::'):]

    def is_wrapped(self, id):
        return str(id) in self.wrapped

    def mentions(self, ty):
        return any(p in self.wrapped for p in self.doc.type_paths(ty))

    # ---- rendering types

    def ty(self, t, our, self_ty=None):
        """Renders a type, with wrapped types as ours (`our`) or jiff's."""
        if t is None:
            return '()'
        k = next(iter(t))
        v = t[k]
        if k == 'resolved_path':
            path = (self.our_path(v['id']) if our and self.is_wrapped(v['id']) else self.jiff_path(v['id']))
            return path + self.args(v.get('args'), our, self_ty)
        if k == 'borrowed_ref':
            lt = (v['lifetime'] + ' ') if v['lifetime'] else ''
            m = 'mut ' if v['is_mutable'] else ''
            return f"&{lt}{m}{self.ty(v['type'], our, self_ty)}"
        if k == 'primitive':
            return v
        if k == 'generic':
            if v == 'Self' and self_ty is not None:
                return self_ty[0] if our else self_ty[1]
            return v
        if k == 'tuple':
            if not v:
                return '()'
            inner = ', '.join(self.ty(x, our, self_ty) for x in v)
            return f'({inner},)' if len(v) == 1 else f'({inner})'
        if k == 'slice':
            return f'[{self.ty(v, our, self_ty)}]'
        if k == 'array':
            return f"[{self.ty(v['type'], our, self_ty)}; {v['len']}]"
        if k == 'impl_trait':
            return 'impl ' + ' + '.join(self.bound(b, our, self_ty) for b in v)
        if k == 'dyn_trait':
            bs = [self.path_bound(tr['trait'], our, self_ty) for tr in v['traits']]
            if v.get('lifetime'):
                bs.append(v['lifetime'])
            return 'dyn ' + ' + '.join(bs)
        if k == 'qualified_path':
            st = self.ty(v['self_type'], our, self_ty)
            if v.get('trait'):
                tr = self.path_bound(v['trait'], our, self_ty)
                return f"<{st} as {tr}>::{v['name']}"
            return f"{st}::{v['name']}"
        if k == 'raw_pointer':
            return f"*{'mut' if v['is_mutable'] else 'const'} {self.ty(v['type'], our, self_ty)}"
        if k == 'infer':
            return '_'
        raise ValueError(k)

    def args(self, a, our, self_ty):
        if not a:
            return ''
        if 'angle_bracketed' in a:
            ab = a['angle_bracketed']
            parts = []
            for g in ab['args']:
                if 'type' in g:
                    parts.append(self.ty(g['type'], our, self_ty))
                elif 'lifetime' in g:
                    parts.append(g['lifetime'])
                elif 'const' in g:
                    parts.append(g['const']['expr'])
                else:
                    parts.append('_')
            for c in ab.get('constraints', []):
                b = c['binding']
                if 'equality' in b:
                    parts.append(f"{c['name']} = {self.ty(b['equality']['type'], our, self_ty)}")
                else:
                    parts.append(f"{c['name']}: " + ' + '.join(self.bound(x, our, self_ty) for x in b['constraint']))
            return '<' + ', '.join(parts) + '>' if parts else ''
        if 'parenthesized' in a:
            p = a['parenthesized']
            ins = ', '.join(self.ty(x, our, self_ty) for x in p['inputs'])
            out = f" -> {self.ty(p['output'], our, self_ty)}" if p.get('output') else ''
            return f'({ins}){out}'
        return ''

    def path_bound(self, p, our, self_ty):
        path = (self.our_path(p['id']) if our and self.is_wrapped(p['id']) else self.trait_path(p))
        return path + self.args(p.get('args'), our, self_ty)

    def trait_path(self, p):
        try:
            return self.jiff_path(p['id'])
        except KeyError:
            return p['path']

    def bound(self, b, our, self_ty):
        if 'trait_bound' in b:
            tb = b['trait_bound']
            pre = ''
            if tb.get('generic_params'):
                pre = 'for<' + ', '.join(g['name'] for g in tb['generic_params']) + '> '
            mod = '?' if tb.get('modifier') == 'maybe' else ''
            return pre + mod + self.path_bound(tb['trait'], our, self_ty)
        if 'outlives' in b:
            return b['outlives']
        raise ValueError(b)

    def generics(self, g, our, self_ty, skip_synthetic=True):
        params = []
        for p in g['params']:
            k = p['kind']
            if 'lifetime' in k:
                o = k['lifetime']['outlives']
                params.append(p['name'] + (': ' + ' + '.join(o) if o else ''))
            elif 'type' in k:
                if skip_synthetic and k['type'].get('is_synthetic'):
                    continue
                bs = k['type']['bounds']
                params.append(p['name'] + (': ' + ' + '.join(self.bound(b, our, self_ty) for b in bs) if bs else ''))
            elif 'const' in k:
                params.append(f"const {p['name']}: {self.ty(k['const']['type'], our, self_ty)}")
        where = []
        for w in g['where_predicates']:
            if 'bound_predicate' in w:
                bp = w['bound_predicate']
                pre = ''
                if bp.get('generic_params'):
                    pre = 'for<' + ', '.join(x['name'] for x in bp['generic_params']) + '> '
                where.append(pre + self.ty(bp['type'], our, self_ty) + ': ' + ' + '.join(self.bound(b, our, self_ty) for b in bp['bounds']))
            elif 'lifetime_predicate' in w:
                lp = w['lifetime_predicate']
                where.append(lp['lifetime'] + ': ' + ' + '.join(lp['outlives']))
        return ('<' + ', '.join(params) + '>' if params else ''), (' where ' + ', '.join(where) if where else '')

    # ---- conversions

    def into_bound_target(self, bounds):
        """The wrapped `X` in an `Into<X>` bound, if any."""
        for b in bounds:
            tb = b.get('trait_bound')
            if not tb:
                continue
            tr = tb['trait']
            if tr['path'].split('::')[-1] == 'Into' and tr.get('args'):
                x = tr['args']['angle_bracketed']['args'][0]['type']
                if self.mentions(x):
                    return x
        return None

    def to_jiff(self, e, t, generics_map, self_ty):
        """An expression of jiff's type, from `e` of ours of type `t`."""
        if not self.mentions(t) and not (t.get('generic') == 'Self' and self_ty):
            k = next(iter(t))
            if k == 'generic' and t['generic'] in generics_map:
                x = generics_map[t['generic']]
                if isinstance(x, dict) and 'closure' in x:
                    return self.closure_conv(e, x['closure'])
                return self.into_conv(e, x) if x else e
            if k == 'impl_trait':
                x = self.into_bound_target(t['impl_trait'])
                return self.into_conv(e, x) if x else e
            return e
        k = next(iter(t))
        v = t[k]
        if k == 'generic' and v == 'Self':
            if self_ty[3]:
                return f'{e}.into_jiff()'
            return f'&{e}.0' if self_ty[2] else f'{e}.0'
        if k == 'resolved_path':
            name = v['path'].split('::')[-1]
            if self.is_wrapped(v['id']):
                return f'{e}.into_jiff()' if str(v['id']) in self.enums else f'{e}.0'
            if name in ('Option',):
                inner = v['args']['angle_bracketed']['args'][0]['type']
                return f'{e}.map(|x| {self.to_jiff("x", inner, generics_map, self_ty)})'
            raise ValueError(('to_jiff path', v['path']))
        if k == 'borrowed_ref':
            inner = v['type']
            ik = next(iter(inner))
            if ik == 'resolved_path' and self.is_wrapped(inner['resolved_path']['id']):
                if str(inner['resolved_path']['id']) in self.enums:
                    raise ValueError('enum by ref')
                return f'&mut {e}.0' if v['is_mutable'] else f'&{e}.0'
            if ik == 'generic' and inner['generic'] == 'Self':
                return f'&{e}.0'
            raise ValueError(('to_jiff ref', inner))
        if k == 'tuple':
            self.fresh = getattr(self, 'fresh', 0) + 1
            names = [f't{self.fresh}_{i}' for i in range(len(v))]
            conv = ', '.join(self.to_jiff(n, x, generics_map, self_ty) for n, x in zip(names, v))
            return f'{{ let ({", ".join(names)}) = {e}; ({conv}) }}'
        if k == 'impl_trait':
            x = self.into_bound_target(v)
            if x is not None:
                return self.into_conv(e, x)
            f = self.fn_bound(v)
            if f is not None:
                return self.closure_conv(e, f)
            raise ValueError(('impl_trait', v))
        if k == 'generic' and v in generics_map:
            x = generics_map[v]
            if x is not None and x.get('closure'):
                return self.closure_conv(e, x['closure'])
            return self.into_conv(e, x)
        raise ValueError(('to_jiff', t))

    def into_conv(self, e, x):
        """`e: impl Into<X>` (X ours) as jiff's X."""
        conv = self.to_jiff('v', x, {}, None)
        our = self.ty(x, True)
        return f'{{ let v: {our} = {e}.into(); {conv} }}'

    def fn_bound(self, bounds):
        for b in bounds:
            tb = b.get('trait_bound')
            if tb and tb['trait']['path'].split('::')[-1] in ('Fn', 'FnMut', 'FnOnce') and tb['trait'].get('args') and 'parenthesized' in tb['trait']['args']:
                p = tb['trait']['args']['parenthesized']
                if any(self.mentions(x) for x in p['inputs']) or (p.get('output') and self.mentions(p['output'])):
                    return p
        return None

    def closure_conv(self, e, p):
        """A closure over our types as one over jiff's."""
        names = [f'c{i}' for i in range(len(p['inputs']))]
        args = ', '.join(self.from_jiff(n, x, None) for n, x in zip(names, p['inputs']))
        call = f'f({args})'
        if p.get('output') and self.mentions(p['output']):
            call = self.to_jiff(f'({call})', p['output'], {}, None)
        return f'{{ let mut f = {e}; move |{", ".join(names)}| {call} }}'

    def from_jiff(self, e, t, self_ty):
        """An expression of ours, from `e` of jiff's type `t`."""
        if t is None:
            return e
        k = next(iter(t))
        v = t[k]
        if k == 'generic' and v == 'Self' and self_ty:
            return f'<{self_ty[0]}>::from_jiff({e})'
        if not self.mentions(t):
            return e
        if k == 'resolved_path':
            name = v['path'].split('::')[-1]
            if self.is_wrapped(v['id']):
                return f'{self.our_path(v["id"])}::from_jiff({e})'
            args = v['args']['angle_bracketed']['args'] if v.get('args') else []
            if name == 'Option':
                return f'{e}.map(|x| {self.from_jiff("x", args[0]["type"], self_ty)})'
            if name == 'Result':
                return f'{e}.map(|x| {self.from_jiff("x", args[0]["type"], self_ty)})'
            raise ValueError(('from_jiff path', v['path']))
        if k == 'borrowed_ref':
            inner = v['type']
            ik = next(iter(inner))
            if ik == 'resolved_path' and self.is_wrapped(inner['resolved_path']['id']):
                if str(inner['resolved_path']['id']) in self.enums:
                    raise ValueError('enum ref out')
                return f'{self.our_path(inner["resolved_path"]["id"])}::ref_jiff({e})'
            raise ValueError(('from_jiff ref', inner))
        if k == 'tuple':
            self.fresh = getattr(self, 'fresh', 0) + 1
            names = [f'u{self.fresh}_{i}' for i in range(len(v))]
            conv = ', '.join(self.from_jiff(n, x, self_ty) for n, x in zip(names, v))
            return f'{{ let ({", ".join(names)}) = {e}; ({conv}) }}'
        raise ValueError(('from_jiff', t))

    def generic_into_map(self, g):
        """Type params whose bound is `Into<X>` with X wrapped -> X, or a
        closure over wrapped types -> {'closure': its signature}."""
        m = {}
        for p in g['params']:
            k = p['kind']
            if 'type' in k:
                m[p['name']] = self.into_bound_target(k['type']['bounds'])
                c = self.fn_bound(k['type']['bounds'])
                if c is not None:
                    m[p['name']] = {'closure': c}
        for w in g['where_predicates']:
            bp = w.get('bound_predicate')
            if bp and 'generic' in bp['type']:
                x = self.into_bound_target(bp['bounds'])
                if x is not None:
                    m[bp['type']['generic']] = x
                c = self.fn_bound(bp['bounds'])
                if c is not None:
                    m[bp['type']['generic']] = {'closure': c}
        return m

    # ---- functions

    def fn(self, f, owner_jiff, self_ty, trait_call=None, vis='pub '):
        """Renders a forwarding function: an inherent method of a wrapped
        type (`owner_jiff` its jiff path), a trait method (`trait_call`
        the qualified jiff trait path) or a free function."""
        fun = f['inner']['function']
        sig = fun['sig']
        gen_p, gen_w = self.generics(fun['generics'], True, self_ty)
        gmap = self.generic_into_map(fun['generics'])
        ins = []
        call_args = []
        recv = None
        for name, t in sig['inputs']:
            if name == 'self':
                enum = self_ty[3]
                if t.get('generic') == 'Self':
                    ins.append('self')
                    recv = 'self.into_jiff()' if enum else 'self.0'
                elif 'borrowed_ref' in t:
                    m = t['borrowed_ref']['is_mutable']
                    lt = t['borrowed_ref']['lifetime']
                    ins.append(f"&{lt + ' ' if lt else ''}{'mut ' if m else ''}self")
                    recv = ('self.as_jiff()' if enum else 'self.0')
                else:
                    ins.append(f'self: {self.ty(t, True, self_ty)}')
                    recv = 'self.into_jiff()' if enum else 'self.0'
                continue
            pname = name if name.isidentifier() else f'arg{len(ins)}'
            ins.append(f'{pname}: {self.ty(t, True, self_ty)}')
            call_args.append(self.to_jiff(pname, t, gmap, self_ty))
        out_t = sig['output']
        if out_t and 'borrowed_ref' in out_t and 'resolved_path' in out_t['borrowed_ref']['type'] and str(out_t['borrowed_ref']['type']['resolved_path']['id']) in self.enums:
            inner = out_t['borrowed_ref']['type']
            ret = f' -> {self.ty(inner, True, self_ty)}'
            call0 = None
        else:
            inner = None
            ret = f' -> {self.ty(out_t, True, self_ty)}' if out_t is not None else ''
        header = fun['header']
        const = 'const ' if header.get('is_const') else ''
        uns = 'unsafe ' if header.get('is_unsafe') else ''
        name = f['name']
        if trait_call:
            target = f'{trait_call}::{name}'
            all_args = ([recv] if recv else []) + call_args
            call = f"{target}({', '.join(all_args)})"
        elif recv:
            call = f"{recv}.{name}({', '.join(call_args)})"
        else:
            call = f"{owner_jiff}::{name}({', '.join(call_args)})" if owner_jiff else f"{self.cur_free}({', '.join(call_args)})"
        body = self.from_jiff(f'{call}.clone()', inner, self_ty) if inner is not None else self.from_jiff(call, out_t, self_ty)
        # A const fn may only forward plainly.
        if const and (any('.into()' in a or '.map(' in a or '{' in a or 'into_jiff' in a or 'as_jiff' in a for a in call_args) or '.map(' in body or '{ let' in body or 'into_jiff' in (recv or '') or 'as_jiff' in (recv or '')):
            const = ''
        if const and recv and 'as_jiff' in recv:
            pass
        if trait_call:
            const = ''
        doc = ''
        return f"{doc}    {vis}{const}{uns}fn {name}{gen_p}({', '.join(ins)}){ret}{gen_w} {{\n        {body}\n    }}\n"

    def doc_link(self, owner, name):
        return f'    /// jiff\'s [`{owner.split("::")[-1]}::{name}`]({owner}::{name}).\n'

    def is_deprecated(self, it):
        return it.get('deprecation') is not None

    def is_hidden(self, it):
        return any('doc(hidden)' in str(a) for a in it.get('attrs', []))

    # ---- types

    def generics_decl(self, g):
        p, w = self.generics(g, False, None, skip_synthetic=False)
        return p, w

    def generic_use(self, g):
        names = []
        for p in g['params']:
            names.append(p['name'])
        return '<' + ', '.join(names) + '>' if names else ''

    def struct(self, tid):
        it = self.doc.item(tid)
        jp = self.jiff_path(tid)
        name = jp.split('::')[-1]
        g = next(iter(it['inner'].values()))['generics']
        gp, gw = self.generics(g, False, None, skip_synthetic=False)
        gu = self.generic_use(g)
        enum = tid in self.enums
        our_self = f'{name}{gu}'
        jiff_self = f'{jp}{gu}'
        code = []
        if enum:
            code.append(self.mirror_enum(tid, it, name, jp, gp, gw, gu))
        else:
            code.append(f'/// jiff\'s [`{name}`]({jp}), without its effectful functions.\n'
                        f'#[derive(ref_cast::RefCast)]\n#[repr(transparent)]\npub struct {name}{gp}(pub(crate) {jiff_self}){gw};\n')
            code.append(f'impl{gp} {our_self}{gw} {{\n'
                        f'    #[inline]\n    #[allow(dead_code)]\n    pub(crate) const fn from_jiff(x: {jiff_self}) -> Self {{ Self(x) }}\n'
                        f'    #[inline]\n    #[allow(dead_code)]\n    pub(crate) fn into_jiff(self) -> {jiff_self} {{ self.0 }}\n'
                        f'    #[inline]\n    #[allow(dead_code)]\n    pub(crate) const fn as_jiff(&self) -> &{jiff_self} {{ &self.0 }}\n'
                        f'    #[inline]\n    #[allow(dead_code)]\n    pub(crate) fn as_jiff_mut(&mut self) -> &mut {jiff_self} {{ &mut self.0 }}\n'
                        f'    #[inline]\n    #[allow(dead_code)]\n    pub(crate) fn ref_jiff<\'r>(x: &\'r {jiff_self}) -> &\'r Self {{ <Self as ref_cast::RefCast>::ref_cast(x) }}\n}}\n')
        # interop both ways
        code.append(f'impl{gp} From<{jiff_self}> for {our_self}{gw} {{\n    fn from(x: {jiff_self}) -> Self {{ Self::from_jiff(x) }}\n}}\n'
                    f'impl{gp} From<{our_self}> for {jiff_self}{gw} {{\n    fn from(x: {our_self}) -> Self {{ x.into_jiff() }}\n}}\n')
        # inherent methods
        self_ty = (our_self, jiff_self, False, enum)
        methods = []
        for imp in self.doc.impls(tid):
            i = imp['inner']['impl']
            if i.get('blanket_impl') or i.get('is_synthetic') or i['trait'] is not None:
                continue
            if self_id(i['for']) != tid:
                continue
            for fid in i['items']:
                f = self.doc.item(fid)
                if f['visibility'] == 'public' and 'assoc_const' in f['inner'] and not self.is_deprecated(f):
                    ct = f['inner']['assoc_const']['type']
                    val = self.from_jiff(f'{jp}::{f["name"]}', ct, self_ty)
                    if enum and self.mentions(ct):
                        continue
                    methods.append(f'    /// jiff\'s [`{name}::{f["name"]}`]({jp}::{f["name"]}).\n    pub const {f["name"]}: {self.ty(ct, True, self_ty)} = {val};\n')
                    continue
                if f['visibility'] != 'public' or 'function' not in f['inner']:
                    continue
                if excluded_fn(jp, f['name']) or self.is_deprecated(f) or self.is_hidden(f):
                    continue
                if enum and any(n == 'self' and t.get('generic') != 'Self' and 'borrowed_ref' in t for n, t in f['inner']['function']['sig']['inputs']):
                    pass
                try:
                    methods.append(self.doc_link(jp, f['name']) + self.fn(f, jp, self_ty))
                except ValueError as e:
                    print(f'skip {jp}::{f["name"]}: {e}', file=sys.stderr)
        if methods:
            code.append(f'impl{gp} {our_self}{gw} {{\n' + ''.join(methods) + '}\n')
        # trait impls
        for imp in self.doc.impls(tid):
            i = imp['inner']['impl']
            if i.get('blanket_impl') or i.get('is_synthetic') or i['trait'] is None:
                continue
            if self_id(i['for']) != tid:
                continue
            code.append(self.trait_impl(imp, i, jp))
        return ''.join(c for c in code if c)

    def mirror_enum(self, tid, it, name, jp, gp, gw, gu):
        e = it['inner']['enum']
        variants = []
        to_j = []
        from_j = []
        for vid in e['variants']:
            v = self.doc.item(vid)
            vn = v['name']
            k = v['inner']['variant']['kind']
            if k == 'plain':
                variants.append(f'    /// jiff\'s [`{name}::{vn}`]({jp}::{vn}).\n    {vn},\n')
                to_j.append(f'{name}::{vn} => {jp}::{vn},')
                from_j.append(f'{jp}::{vn} => {name}::{vn},')
            elif 'tuple' in k:
                fts = [self.doc.item(f)['inner']['struct_field'] for f in k['tuple']]
                variants.append(f'    /// jiff\'s [`{name}::{vn}`]({jp}::{vn}).\n    {vn}(' + ', '.join(self.ty(t, True) for t in fts) + '),\n')
                ns = [f'x{j}' for j in range(len(fts))]
                to_j.append(f'{name}::{vn}({", ".join(ns)}) => {jp}::{vn}(' + ', '.join(self.to_jiff(n, t, {}, None) for n, t in zip(ns, fts)) + '),')
                from_j.append(f'{jp}::{vn}({", ".join(ns)}) => {name}::{vn}(' + ', '.join(self.from_jiff(n, t, None) for n, t in zip(ns, fts)) + '),')
            else:
                fs = [self.doc.item(f) for f in k['struct']['fields']]
                body = ''.join(f'        /// See jiff.\n        {f["name"]}: {self.ty(f["inner"]["struct_field"], True)},\n' for f in fs)
                variants.append(f'    /// jiff\'s [`{name}::{vn}`]({jp}::{vn}).\n    {vn} {{\n{body}    }},\n')
                ns = [f['name'] for f in fs]
                to_j.append(f'{name}::{vn} {{ {", ".join(ns)} }} => {jp}::{vn} {{ ' + ', '.join(f'{n}: {self.to_jiff(n, f["inner"]["struct_field"], {}, None)}' for n, f in zip(ns, fs)) + ' },')
                from_j.append(f'{jp}::{vn} {{ {", ".join(ns)} }} => {name}::{vn} {{ ' + ', '.join(f'{n}: {self.from_jiff(n, f["inner"]["struct_field"], None)}' for n, f in zip(ns, fs)) + ' },')
        nonex = any('non_exhaustive' in str(a) for a in it.get('attrs', []))
        derives = [t for t in ('Clone', 'Copy', 'Debug', 'PartialEq', 'Eq', 'Hash', 'PartialOrd', 'Ord') if t in self.trait_names(tid)]
        wild = '\n            _ => unreachable!("a jiff variant this facade does not know: regenerate it"),' if nonex else ''
        return (f'/// jiff\'s [`{name}`]({jp}), without its effectful functions.\n'
                + ('#[non_exhaustive]\n' if nonex else '')
                + (f'#[derive({", ".join(derives)})]\n' if derives else '')
                + f'pub enum {name}{gp}{gw} {{\n' + ''.join(variants) + '}\n'
                f'impl{gp} {name}{gu}{gw} {{\n'
                f'    #[inline]\n    pub(crate) fn from_jiff(x: {jp}{gu}) -> Self {{\n        match x {{\n            ' + '\n            '.join(from_j) + wild + '\n        }\n    }\n'
                f'    #[inline]\n    pub(crate) fn into_jiff(self) -> {jp}{gu} {{\n        match self {{\n            ' + '\n            '.join(to_j) + '\n        }\n    }\n'
                f'    #[inline]\n    #[allow(dead_code)]\n    pub(crate) fn as_jiff(&self) -> {jp}{gu} where Self: Clone {{ self.clone().into_jiff() }}\n'
                '}\n')

    def trait_names(self, tid):
        out = set()
        for imp in self.doc.impls(tid):
            i = imp['inner']['impl']
            if i['trait'] and self_id(i['for']) == tid and not i.get('blanket_impl'):
                out.add(i['trait']['path'].split('::')[-1])
        return out

    def trait_impl(self, imp, i, jp):
        tr = i['trait']
        tname = tr['path'].split('::')[-1]
        if tname in SKIP_TRAITS:
            return ''
        if self_id(i['for']) in self.enums and tname in ('Clone', 'Copy', 'Debug', 'PartialEq', 'Eq', 'Hash', 'PartialOrd', 'Ord'):
            return ''
        if (jp, tname) in EXCLUDED_IMPLS:
            # Only some TryFrom impls are effectful.
            if tname == 'TryFrom':
                a = tr['args']['angle_bracketed']['args'][0]['type']
                if 'SystemTime' in json.dumps(a):
                    return ''
            else:
                return ''
        gp, gw = self.generics(i['generics'], True, None, skip_synthetic=False)
        for_our = self.ty(i['for'], True)
        for_jiff = self.ty(i['for'], False)
        by_ref = 'borrowed_ref' in i['for']
        trait_our = self.path_bound(tr, True, None)
        trait_jiff = self.path_bound(tr, False, None)
        self_ty = (for_our, for_jiff, by_ref, self_id(i['for']) in self.enums)
        items = []
        for fid in i['items']:
            it = self.doc.item(fid)
            k = next(iter(it['inner']))
            if k == 'assoc_type':
                t = it['inner']['assoc_type']['type']
                items.append(f"    type {it['name']} = {self.ty(t, True, self_ty)};\n")
            elif k == 'assoc_const':
                items.append(f"    const {it['name']}: {self.ty(it['inner']['assoc_const']['type'], True, self_ty)} = <{for_jiff} as {trait_jiff}>::{it['name']};\n")
            elif k == 'function':
                try:
                    items.append(self.trait_fn(it, f'<{for_jiff} as {trait_jiff}>', self_ty, by_ref))
                except ValueError as e:
                    print(f'skip impl {tname} for {for_jiff}: {e}', file=sys.stderr)
                    return ''
        if tname in ('Copy', 'Eq'):
            items = []
        unsafe = 'unsafe ' if i.get('is_unsafe') else ''
        neg = '!' if i.get('is_negative') else ''
        return f'{unsafe}impl{gp} {neg}{trait_our} for {for_our}{gw} {{\n' + ''.join(items) + '}\n'

    def trait_fn(self, f, qual, self_ty, by_ref):
        fun = f['inner']['function']
        sig = fun['sig']
        gen_p, gen_w = self.generics(fun['generics'], True, self_ty)
        gmap = self.generic_into_map(fun['generics'])
        ins = []
        args = []
        for name, t in sig['inputs']:
            if name == 'self':
                enum = self_ty[3]
                if t.get('generic') == 'Self':
                    ins.append('self')
                    args.append('&self.0' if by_ref else ('self.into_jiff()' if enum else 'self.0'))
                elif 'borrowed_ref' in t:
                    m = t['borrowed_ref']['is_mutable']
                    lt = t['borrowed_ref']['lifetime']
                    ins.append(f"&{lt + ' ' if lt else ''}{'mut ' if m else ''}self")
                    if by_ref:
                        args.append('&&self.0')
                    elif enum:
                        args.append('&self.as_jiff()')
                    else:
                        args.append('&mut self.0' if m else '&self.0')
                continue
            pname = name if name.isidentifier() else f'arg{len(ins)}'
            ins.append(f'{pname}: {self.ty(t, True, self_ty)}')
            args.append(self.to_jiff(pname, t, gmap, self_ty))
        out = sig['output']
        ret = f' -> {self.ty(out, True, self_ty)}' if out is not None else ''
        call = f"{qual}::{f['name']}({', '.join(args)})"
        body = self.from_jiff(call, out, self_ty)
        return f"    fn {f['name']}{gen_p}({', '.join(ins)}){ret}{gen_w} {{\n        {body}\n    }}\n"

    # ---- conversions onto kept jiff types

    def kept_froms(self, tid):
        """`From<ours>` for kept jiff types that take wrapped values."""
        jp = self.jiff_path(tid)
        out = []
        for imp in self.doc.impls(tid):
            i = imp['inner']['impl']
            if i.get('blanket_impl') or i.get('is_synthetic') or i['trait'] is None:
                continue
            if self_id(i['for']) != tid:
                continue
            tname = i['trait']['path'].split('::')[-1]
            if tname not in ('From', 'TryFrom'):
                continue
            a = i['trait']['args']['angle_bracketed']['args'][0]['type']
            if not self.mentions(a):
                continue
            gp, gw = self.generics(i['generics'], True, None, skip_synthetic=False)
            for_t = self.ty(i['for'], False)
            a_our = self.ty(a, True)
            conv = self.to_jiff('x', a, {}, None)
            if tname == 'From':
                out.append(f'impl{gp} From<{a_our}> for {for_t}{gw} {{\n    fn from(x: {a_our}) -> Self {{ <{for_t}>::from({conv}) }}\n}}\n')
            else:
                out.append(f'impl{gp} TryFrom<{a_our}> for {for_t}{gw} {{\n    type Error = jiff::Error;\n'
                           f'    fn try_from(x: {a_our}) -> Result<Self, jiff::Error> {{ <{for_t}>::try_from({conv}) }}\n}}\n')
        return ''.join(out)

    # ---- the whole facade

    def module_of(self, path):
        parts = path.split('::')[1:-1]
        return '::'.join(parts)

    def run(self):
        mods = {}  # module path ('' root) -> code
        def add(mod, code):
            mods.setdefault(mod, []).append(code)
        for path, tid in sorted(self.types.items()):
            mod = self.module_of(path)
            if path in MANUAL:
                add(mod, f'pub use {MANUAL[path]};\n')
            elif tid in self.wrapped:
                add(mod, self.struct(tid))
            else:
                add(mod, f'pub use {path};\n')
                add(mod, self.kept_froms(tid))
        # free functions, traits, macros
        for id, path in sorted(self.pp.items(), key=lambda x: x[1]):
            it = self.doc.item(id)
            k = next(iter(it['inner']))
            mod = self.module_of(path)
            if k == 'function':
                if path in EXCLUDED_FREE:
                    continue
                fun = it['inner']['function']
                if self.mentions(fun['sig']):
                    self.cur_free = path
                    add(mod, f'/// jiff\'s [`{path.split("::")[-1]}`]({path}).\n' + self.fn(it, None, None).replace('    pub ', 'pub ', 1).replace('\n    ', '\n'))
                else:
                    add(mod, f'pub use {path};\n')
            elif k == 'trait':
                if path == 'jiff::ToSpan':
                    add(mod, self.to_span(id, it))
                else:
                    add(mod, f'pub use {path};\n')
        return mods

    def to_span(self, id, it):
        """`ToSpan`, giving our `Span`."""
        t = it['inner']['trait']
        items = []
        impls_for = []
        for fid in t['items']:
            f = self.doc.item(fid)
            if 'function' not in f['inner'] or self.is_deprecated(f):
                continue
            sig = f['inner']['function']['sig']
            ins = ', '.join(['self' if n == 'self' and x.get('generic') == 'Self' else ('&self' if n == 'self' else f'{n}: {self.ty(x, True)}') for n, x in sig['inputs']])
            items.append((f['name'], ins, self.ty(sig['output'], True)))
        for impl_id in t['implementations']:
            i = self.doc.item(impl_id)['inner']['impl']
            impls_for.append(self.ty(i['for'], False))
        decl = ''.join(f'    /// jiff\'s [`ToSpan::{n}`](jiff::ToSpan::{n}).\n    fn {n}({ins}) -> {o};\n' for n, ins, o in items)
        out = f'/// jiff\'s [`ToSpan`](jiff::ToSpan), giving this crate\'s [`Span`].\npub trait ToSpan: Sized {{\n{decl}}}\n'
        for ft in impls_for:
            body = ''.join(f'    fn {n}({ins}) -> {o} {{ crate::Span::from_jiff(<{ft} as jiff::ToSpan>::{n}(self)) }}\n' for n, ins, o in items)
            out += f'impl ToSpan for {ft} {{\n{body}}}\n'
        return out


def main():
    doc = Doc(sys.argv[1])
    g = Gen(doc)
    mods = g.run()
    header = ('// @generated by scripts/jiff-facade/generate.py from jiff ' + doc.d.get('crate_version', '') +
              '. Do not edit; regenerate.\n')
    order = sorted(mods, key=lambda m: (m.count('::'), m))
    tree = {}
    for m in order:
        tree[m] = ''.join(mods[m])
    # nest modules
    def render(prefix, depth):
        code = tree.get(prefix, '')
        subs = sorted({m for m in tree if m.startswith(prefix + ('::' if prefix else '')) and m != prefix and m[len(prefix) + (2 if prefix else 0):].count('::') == 0})
        for s in subs:
            name = s.split('::')[-1]
            code += f'\n/// jiff\'s [`{name}`](jiff::{s}) module, without its effectful functions.\npub mod {name} {{\n    #[allow(unused_imports)]\n    use super::*;\n' + render(s, depth + 1) + '}\n'
        return code
    print(header + '#![allow(clippy::all, clippy::pedantic, unused_qualifications, missing_docs)]\n' + render('', 0))


if __name__ == '__main__':
    main()
