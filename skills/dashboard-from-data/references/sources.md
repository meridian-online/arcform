# Method sources

The rules in this skill distil the method of the work below. Read them for method when a rule needs its reasoning; none of their text is copied here, and none of it should be copied into a dashboard or a reply. OpenAI's data-analytics plugin is proprietary: take nothing from it but the ideas named here.

## Agent skills

- **Anthropic's data plugin**: profile first, and read each column's role (measure, dimension, identifier, time) from the profile before choosing what to chart.
- **OpenAI's visualisation-strategy skill**: state the claim before composing the views, and its anti-patterns (equal-weight tiles, charts with no takeaway).
- **OpenAI's data-analytics plugin**: "no fixed checklist", so exploration follows the data rather than a list of steps.
- **MotherDuck's skills**: "defaults, not quotas", so a default view count or chart kind is never a number to fill.
- **Rill's analysis skills**: the query loop, each query chosen from the last result, and the rule that every number stated comes from an executed query.
- **Databricks' analysis skills**: confirm the story against the data before presenting it.
- **The data-storytelling and EDA skills in the public skills registries**: the honesty gates, a takeaway in one sentence, nothing claimed as causal that is only associated.

None of these says which view in a crossfilter should drive which. The focal-view and responder rules in `composing.md` are this skill's own.

## Behind them

- **John Tukey, *Exploratory Data Analysis***: look at the data before you model it, and let what you see choose the next look.
- **William Cleveland and Robert McGill, "Graphical Perception"**: position on a common scale reads more accurately than length, angle or colour, so the focal view puts the claim on a position channel.
- **Tamara Munzner, *Visualization Analysis and Design***: choose views by the task and the data's types, and link them so one view's selection shows in the others.
- **Ben Shneiderman, "The Eyes Have It"**: overview first, zoom and filter, then details on demand; the grid is the details.
- **Eric Horvitz, "Principles of Mixed-Initiative User Interfaces", and Jeffrey Heer, "Agency plus automation"**: for checking back, an agent acts where it can, says what it assumed, and leaves the user able to check and override it.
