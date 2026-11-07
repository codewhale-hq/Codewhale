# Mistral Vibe Code

Codewhale's `mistral` provider can be configured with a Vibe Code API key. An
actual Codewhale conversation using that key has not yet been verified. You do
not need a separate provider or a Vibe process to configure the key.

1. In [Mistral Code > Vibe CLI](https://chat.mistral.ai/code/extensions),
   create a Vibe Code API key. Mistral documents this key for Free mode, paid
   plans and pay-as-you-go. Keep the key out of prompts and shell history.
2. Run `codewhale auth set --provider mistral` and enter the key when prompted,
   or choose Mistral in `/provider` and enter it there. Alternatively, provide
   it to the process through `MISTRAL_API_KEY`. A saved Codewhale credential
   takes precedence over the environment variable; an explicit `--api-key`
   takes precedence for that launch.
3. Start `codewhale --provider mistral --model mistral-medium-latest`, or select
   Mistral and the model in the provider and model pickers. Mistral recommends
   `mistral-medium-latest` for reasoning and code. Model access depends on the
   account and plan. The default API base is `https://api.mistral.ai/v1`.

Mistral says included monthly usage is shared across Studio, the API and Vibe
Code. When it runs out, requests may stop or use prepaid pay-as-you-go credits,
depending on the organization's settings. Check the [subscription
settings](https://admin.mistral.ai/subscription) before use. Codewhale's
token-price estimates are not an invoice, remaining allowance or proof of a
subscription charge. Mistral's [key and billing
guide](https://docs.mistral.ai/vibe/code/cli/api-keys-profiles) has current
details. Codewhale does not automatically import Vibe's keyring or sign in
through Vibe's browser flow.
