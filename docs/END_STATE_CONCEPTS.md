# End State Concepts

Not a spec, not something we're building yet. This is me writing down where I actually want this to end up, before it gets buried under kernel plumbing and I lose the thread of why any of this matters.

Rose being a clean capability microkernel that boots and runs components is the foundation, not the goal. What I actually want is a system that understands itself, understands the person using it, and helps without getting in the way or turning into the bloated, commercialized mess that basically every OS ends up being eventually. No telemetry phoning home, no ads, no forced accounts, no background junk running because some business model needed it there. If it's running, it's running because it earns its place.

## Two layers, not one

I keep coming back to splitting this into two pieces instead of one big "AI in the system" idea, because they don't carry the same risk and shouldn't be treated like they do.

The first piece is onboard diagnostics. I'm calling it Stem for now, since it fits the Rose naming and it fits the job — it feeds raw material upward and doesn't decide anything with it. Stem watches hardware and software state, keeps a running baseline for whatever it's tracking, and periodically checks if something has moved outside its normal range. That's it. It doesn't interpret what a change means, it doesn't guess at causes, it just notices "this is different than it usually is" and says so. Collected, not judged, compared for changes worth a second look. When something crosses that line, Stem pushes it immediately, interrupt style, rather than waiting around for someone to come ask. It doesn't sit on a problem.

The second piece is Compass. This is the one that actually thinks about what Stem hands it. Compass decides whether a flagged change is actually worth caring about or just noise, figures out what's probably going on, and if it's something worth doing something about, it talks to me about it instead of just doing it. Compass is also the piece that's supposed to understand what I'm actually working on and help with that — not just system health, but goals and workflow. Two jobs, one layer, because they both come from the same place: actually paying attention to what's happening and what I need.

## How it's allowed to act

Compass doesn't get to just do things. It asks. Something like:

"I have noticed CPU temperatures are higher than usual. You have a program running in the background that isn't vital to any work you are currently doing. Suspending or terminating it should help bring temps back down and restore some performance. Would you like me to handle that?"

That's the shape every interaction should take — what it noticed, what it thinks is going on, what it wants to do, what I should expect if it does it, and then it waits for me to say yes. Saying yes is the only way it gets the authority to act. It doesn't hold onto that authority afterward either, unless I tell it to remember the decision for next time. So there are really three ways this goes: handle it once and forget it, handle it and remember to do the same thing automatically next time this exact situation comes up, or don't ask about this again at all. All three are things I control, not things it decides for itself.

## Where it learns from

The only training signal I want feeding this is whatever I actually tell it when it asks — accepted, declined, or "do it differently." Nothing external, nothing phoned out, nothing collected beyond what's sitting on the machine already. If it gets smarter about what to suggest, it's because I taught it, not because some server somewhere aggregated my usage with everyone else's.

## It can't become what it's watching for

Whatever runs Stem and Compass has to live inside its own bounded footprint, same as anything else on the system. The entire point of this is catching things that are dragging the system down or wasting resources — it would be a joke if the thing doing the catching became the thing that needed catching. So whatever authority either of these gets, part of that authority is a hard limit on what it's allowed to cost.

## It's optional

None of this is core to Rose in the sense that Rose can't exist without it. A build of Rose with neither Stem nor Compass in it should still be a complete, working operating system. This is a layer you can leave out, not a tax you have to pay to run the thing at all.

## Not yet

None of this can actually get built until the kernel has capabilities, IPC, and a scheduler working, because right now there's nothing running for Stem to watch or for Compass to help with. This is here so the idea doesn't get lost between now and whenever that foundation is actually ready.
