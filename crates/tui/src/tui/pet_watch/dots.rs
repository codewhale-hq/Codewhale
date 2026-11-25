//! This terminal's own dot whale, projected from its own session's typed
//! Engine metadata by the bundled reducer and dot field.
//!
//! Known limits: events before the first `/whale` in a process are not seen;
//! the frame uses the dot field's raw pigment. Any failure leaves the view on
//! its fallback.
use rquickjs::{Context, Function, Runtime};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    mpsc::{self, RecvTimeoutError, Sender},
};
use std::time::{Duration, Instant};

const POINTS: usize = 980;
const PENDING: usize = 4096;
const PRELUDE: &str = r#"(function(){
const F=globalThis.PetDotField,E=globalThis.PetEngineTelemetry;
if(!F||typeof F.projectDotField!=='function'||typeof F.blendDotFields!=='function'||typeof F.dotFieldKey!=='function'||typeof E!=='function')throw Error('Pet dot field unavailable.');
let s;
const reset=()=>{s={engine:new E(),at:0,from:undefined,last:undefined,key:'',still:''};};
reset();
const flat=(f,hollow,busy)=>{
if(!f||!Array.isArray(f.points)||!Array.isArray(f.materials)||f.points.length!==980||f.materials.length!==980)throw Error('Invalid pet dot field.');
const o=new Array(2+980*6);o[0]=hollow?1:0;o[1]=busy?1:0;
for(let i=0;i<980;i++){const p=f.points[i],m=f.materials[i],k=2+i*6;o[k]=p[0];o[k+1]=p[1];o[k+2]=m[0];o[k+3]=m[1];o[k+4]=m[2];o[k+5]=m[3];}
return o;};
globalThis.localReset=reset;
globalThis.localForget=()=>{s.still='';};
globalThis.localFresh=(t)=>s.engine.lastObservedAt!==undefined&&t-s.engine.lastObservedAt<=12500;
globalThis.localObserve=(json,t)=>{
const ev=JSON.parse(json);if(!Array.isArray(ev))throw Error('Invalid pet batch.');
const n=s.engine.clone();
try{for(const e of ev)n.observe(e,t);s.engine=n;return;}catch(_){}
for(const e of ev){const m=s.engine.clone();try{m.observe(e,t);s.engine=m;}catch(_){}}};
globalThis.localFrame=(t,still,waiting)=>{
s.engine.confirmWaiting(t,waiting);
const a=s.engine.activity(t),o={bodyStyle:'smooth'},hollow=!(a.observed&&a.freshness==='fresh');
const k=F.dotFieldKey(a);
if(k!==s.key){if(s.key&&s.last){s.from=s.last;s.at=t;}s.key=k;}
if(still){
const done=!hollow&&a.authoritativePresence==='done'&&t-(a.observedAtMs??t)<2600;
const sk=k+':'+hollow+':'+done;
if(sk===s.still)return null;
const r=flat(F.projectDotField(a,t,true,0,o),hollow,false);
s.still=sk;
return r;}
s.still='';
const target=F.projectDotField(a,t,false,s.at,o);
const f=s.from?F.blendDotFields(s.from,target,(t-s.at)/560):target;
const busy=!!s.from||target.morph>0;
if(s.from&&t-s.at>=560)s.from=undefined;
s.last=f;
return flat(f,hollow,busy);};
})();"#;

const BUNDLE: &str = include_str!("pet-native.js");

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Frame {
    pub points: Vec<[f64; 2]>,
    pub materials: Vec<[f64; 4]>,
    pub hollow: bool,
    pub busy: bool,
}

impl Frame {
    fn from_flat(flat: &[f64]) -> Option<Self> {
        if flat.len() != 2 + POINTS * 6 || !flat.iter().all(|v| v.is_finite()) {
            return None;
        }
        let mut points = Vec::with_capacity(POINTS);
        let mut materials = Vec::with_capacity(POINTS);
        for &[x, y, r, g, b, a] in flat[2..].as_chunks::<6>().0 {
            let point = [x, y];
            let material = [r, g, b, a];
            if !point.iter().all(|v| (-1.0..=1.0).contains(v))
                || !material[..3].iter().all(|v| (0.0..=255.0).contains(v))
                || !(0.0..=1.0).contains(&material[3])
            {
                return None;
            }
            points.push(point);
            materials.push(material);
        }
        Some(Self {
            points,
            materials,
            hollow: flat[0] != 0.0,
            busy: flat[1] != 0.0,
        })
    }
}

pub(super) struct Projector {
    context: Context,
    _runtime: Runtime,
    deadline: Arc<Mutex<Instant>>,
}

impl Projector {
    #[cfg(test)]
    pub(super) fn new() -> Option<Self> {
        Self::from_source(BUNDLE)
    }

    fn from_source(source: &str) -> Option<Self> {
        let runtime = Runtime::new().ok()?;
        runtime.set_memory_limit(64 * 1024 * 1024);
        runtime.set_max_stack_size(2 * 1024 * 1024);
        let deadline = Arc::new(Mutex::new(Instant::now() + Duration::from_secs(10)));
        let limit = Arc::clone(&deadline);
        runtime.set_interrupt_handler(Some(Box::new(move || {
            limit.lock().map_or(true, |d| Instant::now() > *d)
        })));
        let context = Context::full(&runtime).ok()?;
        context
            .with(|ctx| -> rquickjs::Result<()> {
                ctx.eval::<(), _>(source)?;
                ctx.eval::<(), _>(PRELUDE)
            })
            .ok()?;
        Some(Self {
            context,
            _runtime: runtime,
            deadline,
        })
    }

    fn arm(&self, limit: Duration) -> bool {
        self.deadline
            .lock()
            .map(|mut d| *d = Instant::now() + limit)
            .is_ok()
    }

    pub(super) fn observe(&self, json: &str, t: f64) -> bool {
        self.arm(Duration::from_secs(5))
            && self.context.with(|ctx| {
                let result = ctx
                    .globals()
                    .get::<_, Function>("localObserve")
                    .and_then(|f| f.call::<_, ()>((json, t)));
                if result.is_err() {
                    let _ = ctx.catch();
                }
                result.is_ok()
            })
    }

    pub(super) fn reset(&self) -> bool {
        self.arm(Duration::from_secs(1))
            && self.context.with(|ctx| {
                let result = ctx
                    .globals()
                    .get::<_, Function>("localReset")
                    .and_then(|f| f.call::<_, ()>(()));
                if result.is_err() {
                    let _ = ctx.catch();
                }
                result.is_ok()
            })
    }

    pub(super) fn frame(&self, t: f64, still: bool, waiting: bool) -> Result<Option<Frame>, ()> {
        if !self.arm(Duration::from_secs(60)) {
            return Err(());
        }
        let flat = self.context.with(|ctx| {
            let result = ctx
                .globals()
                .get::<_, Function>("localFrame")
                .and_then(|f| f.call::<_, Option<Vec<f64>>>((t, still, waiting)));
            if result.is_err() {
                let _ = ctx.catch();
            }
            result.map_err(|_| ())
        })?;
        match flat.map(|flat| Frame::from_flat(&flat)) {
            None => Ok(None),
            Some(Some(frame)) => Ok(Some(frame)),
            Some(None) => {
                self.forget();
                Err(())
            }
        }
    }

    fn fresh(&self, t: f64) -> bool {
        self.arm(Duration::from_secs(1))
            && self.context.with(|ctx| {
                let result = ctx
                    .globals()
                    .get::<_, Function>("localFresh")
                    .and_then(|f| f.call::<_, bool>((t,)));
                if result.is_err() {
                    let _ = ctx.catch();
                }
                result.unwrap_or(true)
            })
    }

    fn forget(&self) {
        if self.arm(Duration::from_secs(1)) {
            self.context.with(|ctx| {
                let result = ctx
                    .globals()
                    .get::<_, Function>("localForget")
                    .and_then(|f| f.call::<_, ()>(()));
                if result.is_err() {
                    let _ = ctx.catch();
                }
            });
        }
    }
}

#[derive(Clone, Copy)]
struct View {
    visible: bool,
    motion: bool,
    waiting: bool,
}

enum Msg {
    Observe(String),
    Reset(u64),
    Wake,
}

pub(super) struct Dots {
    tx: Sender<Msg>,
    pending: Arc<AtomicUsize>,
    gap: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    view: Arc<Mutex<View>>,
    latest: Arc<Mutex<Option<(u64, Frame)>>>,
    failed: Arc<AtomicBool>,
}

struct Shared {
    pending: Arc<AtomicUsize>,
    gap: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    view: Arc<Mutex<View>>,
    latest: Arc<Mutex<Option<(u64, Frame)>>>,
}

impl Dots {
    pub(super) fn start() -> Self {
        Self::start_from(BUNDLE)
    }

    fn start_from(source: &'static str) -> Self {
        let (tx, rx) = mpsc::channel();
        let dots = Self {
            tx,
            pending: Arc::new(AtomicUsize::new(0)),
            gap: Arc::new(AtomicBool::new(false)),
            generation: Arc::new(AtomicU64::new(0)),
            view: Arc::new(Mutex::new(View {
                visible: false,
                motion: false,
                waiting: false,
            })),
            latest: Arc::new(Mutex::new(None)),
            failed: Arc::new(AtomicBool::new(false)),
        };
        let shared = Shared {
            pending: Arc::clone(&dots.pending),
            gap: Arc::clone(&dots.gap),
            generation: Arc::clone(&dots.generation),
            view: Arc::clone(&dots.view),
            latest: Arc::clone(&dots.latest),
        };
        let failed = Arc::clone(&dots.failed);
        let spawned = std::thread::Builder::new()
            .name("pet-dots".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let projector = Projector::from_source(source).ok_or(())?;
                    run(&projector, &rx, &shared)
                }));
                if !matches!(ran, Ok(Ok(()))) {
                    failed.store(true, Ordering::Release);
                }
            });
        if spawned.is_err() {
            dots.failed.store(true, Ordering::Release);
        }
        dots
    }

    pub(super) fn observe(&self, json: String) {
        if self.pending.fetch_add(1, Ordering::AcqRel) >= PENDING {
            self.pending.fetch_sub(1, Ordering::AcqRel);
            self.gap.store(true, Ordering::Release);
        } else if self.tx.send(Msg::Observe(json)).is_err() {
            self.pending.fetch_sub(1, Ordering::AcqRel);
        }
    }

    pub(super) fn reset(&self) {
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        if let Ok(mut latest) = self.latest.lock() {
            *latest = None;
        }
        let _ = self.tx.send(Msg::Reset(generation));
    }

    pub(super) fn set_view(&self, visible: bool, motion: bool, waiting: bool) {
        let woke = self.view.lock().is_ok_and(|mut view| {
            let next = View {
                visible,
                motion,
                waiting,
            };
            let woke = visible
                && (!view.visible || view.motion != next.motion || view.waiting != next.waiting);
            *view = next;
            woke
        });
        if woke {
            let _ = self.tx.send(Msg::Wake);
        }
    }

    pub(super) fn take(&self) -> Option<Frame> {
        let current = self.generation.load(Ordering::Acquire);
        self.latest
            .lock()
            .ok()
            .and_then(|mut latest| latest.take())
            .and_then(|(generation, frame)| (generation == current).then_some(frame))
    }

    pub(super) fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
}

fn run(projector: &Projector, rx: &mpsc::Receiver<Msg>, shared: &Shared) -> Result<(), ()> {
    let origin = Instant::now();
    let mut due = origin;
    let mut errors = 0u8;
    let mut applied = 0u64;
    let mut batch: Vec<String> = Vec::new();
    let mut reset = false;
    let mut force = true;
    let mut settled = false;
    loop {
        let visible = shared.view.lock().map_err(|_| ())?.visible;
        let first = if (!visible || settled) && batch.is_empty() && !reset && !force {
            rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
        } else {
            rx.recv_timeout(due.saturating_duration_since(Instant::now()))
        };
        let mut next = match first {
            Ok(msg) => Some(msg),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        };
        while let Some(msg) = next.take() {
            match msg {
                Msg::Observe(json) => {
                    shared.pending.fetch_sub(1, Ordering::AcqRel);
                    batch.push(json);
                }
                Msg::Reset(generation) => {
                    batch.clear();
                    reset = true;
                    force = true;
                    applied = generation;
                }
                Msg::Wake => force = true,
            }
            next = rx.try_recv().ok();
        }
        if shared.gap.swap(false, Ordering::AcqRel) {
            batch.clear();
            reset = true;
            applied = shared.generation.load(Ordering::Acquire);
        }
        if !force && Instant::now() < due {
            continue;
        }
        force = false;
        let t = origin.elapsed().as_secs_f64() * 1000.0;
        if std::mem::take(&mut reset) {
            projector.reset();
        }
        if !batch.is_empty() {
            projector.observe(&format!("[{}]", batch.join(",")), t);
            batch.clear();
        }
        let view = *shared.view.lock().map_err(|_| ())?;
        let busy = if !view.visible {
            false
        } else {
            match projector.frame(t, !view.motion, view.waiting) {
                Ok(Some(frame)) => {
                    errors = 0;
                    let busy = frame.busy;
                    *shared.latest.lock().map_err(|_| ())? = Some((applied, frame));
                    busy
                }
                Ok(None) => {
                    errors = 0;
                    false
                }
                Err(()) => {
                    errors += 1;
                    if errors >= 3 {
                        return Err(());
                    }
                    false
                }
            }
        };
        settled = view.visible && !view.motion && !projector.fresh(t);
        due = Instant::now()
            + Duration::from_millis(match (view.visible, view.motion) {
                (true, true) if busy => 66,
                (true, false) => 500,
                _ => 250,
            });
    }
}

#[cfg(test)]
pub(super) fn failing_for_test() -> Dots {
    Dots::start_from("throw Error('no dot field');")
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_committed_bundle_exports_the_reducer_and_the_dot_field() {
        assert!(super::Projector::from_source(super::BUNDLE).is_some());
        assert!(super::Projector::from_source("globalThis.PetDotField={};").is_none());
    }
}
