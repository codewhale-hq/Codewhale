//! This terminal's own dot whale, projected from its own session's typed
//! Engine metadata by the bundled reducer and dot field. The companion keeps
//! identity, clock, recording, sound and the browser view.
//!
//! Known limits: events before the first `/pet` in a process are not seen;
//! the local frame uses raw pigment, without the companion's brightness,
//! particle colour or doze dimming; the body style follows the last companion
//! frame this terminal received. Any failure leaves the view on its fallback.
use rquickjs::{Context, Function, Runtime};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, RecvTimeoutError, SyncSender, TrySendError},
};
use std::time::{Duration, Instant};

const POINTS: usize = 980;
const ANCHOR: &str = "global.PetNative=load('pet-native').PetNative;";
const EXPORTS: &str = "global.PetDotField=load('dot-field');global.PetEngineTelemetry=load('pet-engine').PetEngineTelemetry;";
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
globalThis.localObserve=(json,t)=>{const n=s.engine.clone();n.observe(JSON.parse(json),t);s.engine=n;};
globalThis.localFrame=(t,still,waiting,body)=>{
s.engine.confirmWaiting(t,waiting);
const a=s.engine.activity(t),o={bodyStyle:body},hollow=!(a.observed&&a.freshness==='fresh');
const k=F.dotFieldKey(a)+':'+body;
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

fn bundle() -> Option<String> {
    let source = include_str!("pet-native.js");
    (source.matches(ANCHOR).count() == 1)
        .then(|| source.replacen(ANCHOR, &format!("{ANCHOR}{EXPORTS}"), 1))
}

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
    pub(super) fn new() -> Option<Self> {
        let source = bundle()?;
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
        self.arm(Duration::from_secs(1))
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

    pub(super) fn frame(
        &self,
        t: f64,
        still: bool,
        waiting: bool,
        body: &str,
    ) -> Result<Option<Frame>, ()> {
        if !self.arm(Duration::from_secs(20)) {
            return Err(());
        }
        let flat = self.context.with(|ctx| {
            let result = ctx
                .globals()
                .get::<_, Function>("localFrame")
                .and_then(|f| f.call::<_, Option<Vec<f64>>>((t, still, waiting, body)));
            if result.is_err() {
                let _ = ctx.catch();
            }
            result.map_err(|_| ())
        })?;
        flat.map(|flat| Frame::from_flat(&flat).ok_or(()))
            .transpose()
    }
}

#[derive(Clone, Copy)]
struct View {
    visible: bool,
    motion: bool,
    waiting: bool,
    body: &'static str,
}

enum Msg {
    Observe(String),
    Reset,
    Wake,
}

pub(super) struct Dots {
    tx: SyncSender<Msg>,
    gap: Arc<AtomicBool>,
    view: Arc<Mutex<View>>,
    latest: Arc<Mutex<Option<Frame>>>,
    failed: Arc<AtomicBool>,
}

impl Dots {
    pub(super) fn start() -> Self {
        let (tx, rx) = mpsc::sync_channel(256);
        let dots = Self {
            tx,
            gap: Arc::new(AtomicBool::new(false)),
            view: Arc::new(Mutex::new(View {
                visible: false,
                motion: false,
                waiting: false,
                body: "smooth",
            })),
            latest: Arc::new(Mutex::new(None)),
            failed: Arc::new(AtomicBool::new(false)),
        };
        let (gap, view, latest, failed) = (
            Arc::clone(&dots.gap),
            Arc::clone(&dots.view),
            Arc::clone(&dots.latest),
            Arc::clone(&dots.failed),
        );
        let spawned = std::thread::Builder::new()
            .name("pet-dots".into())
            .spawn(move || {
                if run(&rx, &gap, &view, &latest).is_err() {
                    failed.store(true, Ordering::Release);
                }
            });
        if spawned.is_err() {
            dots.failed.store(true, Ordering::Release);
        }
        dots
    }

    fn send(&self, msg: Msg) {
        if let Err(TrySendError::Full(_)) = self.tx.try_send(msg) {
            self.gap.store(true, Ordering::Release);
        }
    }

    pub(super) fn observe(&self, json: String) {
        self.send(Msg::Observe(json));
    }

    pub(super) fn reset(&self) {
        if let Ok(mut latest) = self.latest.lock() {
            *latest = None;
        }
        self.send(Msg::Reset);
    }

    pub(super) fn set_view(
        &self,
        visible: bool,
        motion: bool,
        waiting: bool,
        body: Option<&'static str>,
    ) {
        let woke = self.view.lock().is_ok_and(|mut view| {
            let woke = visible && !view.visible;
            *view = View {
                visible,
                motion,
                waiting,
                body: body.unwrap_or(view.body),
            };
            woke
        });
        if woke {
            let _ = self.tx.try_send(Msg::Wake);
        }
    }

    pub(super) fn take(&self) -> Option<Frame> {
        self.latest.lock().ok().and_then(|mut latest| latest.take())
    }

    pub(super) fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
}

fn run(
    rx: &mpsc::Receiver<Msg>,
    gap: &AtomicBool,
    view: &Mutex<View>,
    latest: &Mutex<Option<Frame>>,
) -> Result<(), ()> {
    let projector = Projector::new().ok_or(())?;
    let origin = Instant::now();
    let mut wait: Option<Duration> = None;
    let mut errors = 0u8;
    loop {
        let first = match wait {
            Some(wait) => rx.recv_timeout(wait),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        let mut next = match first {
            Ok(msg) => Some(msg),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        };
        let t = origin.elapsed().as_secs_f64() * 1000.0;
        if gap.swap(false, Ordering::AcqRel) {
            projector.reset();
        }
        while let Some(msg) = next.take() {
            match msg {
                Msg::Observe(json) => {
                    projector.observe(&json, t);
                }
                Msg::Reset => {
                    projector.reset();
                }
                Msg::Wake => {}
            }
            next = rx.try_recv().ok();
        }
        let view = *view.lock().map_err(|_| ())?;
        if !view.visible {
            wait = None;
            continue;
        }
        let busy = match projector.frame(t, !view.motion, view.waiting, view.body) {
            Ok(Some(frame)) => {
                errors = 0;
                let busy = frame.busy;
                *latest.lock().map_err(|_| ())? = Some(frame);
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
        };
        wait = Some(Duration::from_millis(if view.motion && busy {
            66
        } else {
            250
        }));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_committed_bundle_has_exactly_one_export_anchor() {
        assert_eq!(
            include_str!("pet-native.js").matches(super::ANCHOR).count(),
            1
        );
        assert!(super::bundle().is_some_and(|source| source.contains(super::EXPORTS)));
    }
}
