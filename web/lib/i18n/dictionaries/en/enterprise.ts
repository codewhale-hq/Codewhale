export interface EnterpriseDict {
  metaTitle: string;
  metaDescription: string;
  kicker: string;
  status: string;
  title: string;
  lead: string;
  partnerCta: string;
  secondaryCta: string;
  loopTitle: string;
  loopScope: string;
  loop: { title: string; body: string }[];
  statusTitle: string;
  statusScope: string;
  todayTitle: string;
  today: { title: string; body: string }[];
  plannedTitle: string;
  planned: { title: string; body: string }[];
  controlTitle: string;
  dataTitle: string;
  dataBody: string;
  engineTitle: string;
  engineBody: string;
  partnerTitle: string;
  partnerBody: string;
  partnerEmail: string;
}

export const enterprise: EnterpriseDict = {
  metaTitle: "Codewhale Enterprise · In development",
  metaDescription: "Codewhale Enterprise turns your team's work into evals, training data and models you own. In development; open to design partners.",
  kicker: "Codewhale Enterprise",
  status: "In development",
  title: "Own the intelligence your team runs on",
  lead: "Your team's work already runs through Codewhale. Enterprise turns that work into evals, training data and models you own. It is in development and open to design partners.",
  partnerCta: "Become a design partner",
  secondaryCta: "Try the open-source agent",

  loopTitle: "What you get",
  loopScope: "Every model is tested on your own held-out tasks before it replaces anything.",
  loop: [
    {
      title: "Evals that define done",
      body: "An eval suite built from your team's real work, with a held-out set that decides whether a model is good enough.",
    },
    {
      title: "Training data from your work",
      body: "Data drawn from work your team already accepted, with sensitive details removed.",
    },
    {
      title: "A model you own",
      body: "A fine-tuned open-weight model you control, compared with your current model on quality, cost and speed.",
    },
    {
      title: "Environments that keep improving it",
      body: "RL environments built from your own tasks, so your model keeps improving on the work that matters to you.",
    },
  ],

  statusTitle: "Available today and planned",
  statusScope: "The open-source agent is available now. Enterprise is in development, and no customer model has been trained yet.",
  todayTitle: "Available today",
  today: [
    {
      title: "An agent for any model",
      body: "Hosted providers, any OpenAI-compatible endpoint, and models you serve yourself with vLLM, SGLang or Ollama.",
    },
    {
      title: "Saved sessions with receipts",
      body: "Every session keeps its tool calls, results and approvals, so your team's work is on record.",
    },
  ],
  plannedTitle: "Planned for Enterprise",
  planned: [
    {
      title: "Managed eval and training environments",
      body: "Environments that run your evals and training without depending on a laptop.",
    },
    {
      title: "Qualification on every new base model",
      body: "When a better open model ships, it is tested on your holdout and promoted only if it passes.",
    },
    {
      title: "Routing between rented and owned models",
      body: "Each workflow goes to the model, rented or owned, with the best accepted result per cost.",
    },
    {
      title: "Private deployment",
      body: "Run the agent and Enterprise where your data must stay.",
    },
  ],

  controlTitle: "You keep control",
  dataTitle: "Your data stays where you choose",
  dataBody: "Your sessions, datasets and models stay in places you choose. Training runs only on compute you approve.",
  engineTitle: "One Rust engine with approvals",
  engineBody: "The same engine runs your workflows and Enterprise. Ask, Auto-Review and Full Access permissions and receipts apply to both.",

  partnerTitle: "Design partners",
  partnerBody: "Bring one workflow your team repeats. You get a baseline of your current model on your own holdout, then a test of an owned model against it. Email us to start.",
  partnerEmail: "help@codewhale.net",
};
