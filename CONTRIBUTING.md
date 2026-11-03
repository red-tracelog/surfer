# Contributing to Surfer

Thank you for considering contributing to Surfer! To make the process as smooth as possible,
here are a few things to keep in mind:

Surfer is becoming an integral part of the open source EDA ecosystem, both as a popular waveform viewer for users, and as a point of integration for other tools tools such as [slang-server](https://github.com/hudson-trading/slang-server).

Because of this, we are in a position where long term stability and maintainability are important considerations for any change. Combined with limited maintainer time, this means that new features must be both high quality, and broadly useful to be accepted.

Smaller changes and bug fixes are of course always welcome!

## Contributing Features

While we are in principle excited about new features and new ways to look at
waveforms, accepting new features in Surfer means committing to maintaining both the
new code and the functionality, essentially forever. Therefore, if you have a new
idea for a feature, there are two approaches:

- Try to use our existing integration protocols to build it outside Surfer. In particular, the [https://gitlab.com/waveform-control-protocol/wcp](Waveform Control Protocol) is designed to allow external programs to integrate with and control the waveform viewer. If there is something you would like to do with this that is not possible, an extension of the protocols is a much more useful contribution than a singular new feature.
- Discuss the feature request ahead of time in an issue or in our matrix channel. That will allow us to tell you early on whether or not a feature is desired, before you spend time working on it.

## Submitting a Merge Request

In your merge request, it simplifies the review process if you provide a few details where relevant:

- An overview of how your change works. What should we know before reading the code?
- The status of the MR.
    - **Clearly mark drafts**: Opening an MR to get early feedback is great, but reviewing such a change requires a very different set of eyes than a "finished" change.
    - **Outstanding issues**: It is likely that, even in a non-draft MR, you know yourself that some parts of the code need extra work. If you do, let us know so we know where to focus review effort
    - **Keep changes small**: Review effort does not scale linearly with the size of changes. Therefore, please keep your changes small. If you have two features, split them in two MRs. If you want to build on top of your non-merged work, start a new branch on top and a separate MR.


## No LLM Contributions

In short: **Do not submit changes containing LLM generated code**.

To motivate this: Surfer is an established project that much of the open source EDA community now relies on. This means that we as maintainers need to make sure that the quality is high, and continues to be high in the future. Further, maintainer time is increasingly becoming a bottleneck to development speed, and code review is taking up a significant amount of our limited time.

We do code review for a few primary reasons:
- Make sure that new code quality is acceptable
- Make sure that the overall architecture is sound
- Onboarding new developers

We have found that reviewing LLM generated code takes more effort because the mistakes LLMs make are less predictable than the mistakes human programmers make. A novice may write code that is causes future issues, but this is usually easy to spot. LLM generated code often does the same thing, but those issues are harder to spot, meaning that code review goes from a thorough glance to spot potential structural problems, to having to look at every small change in detail. This is unsustainable for us. Having to do this also means loosing sight of the bigger picture, meaning pulling focus away from the more important architectural issues

This is made worse by the fact that code review is also intended to onboard new developers. Reviewing a contribution from a new developer often takes more time than simply writing the code ourselves, so spending the time on review simply to get a new fix or feature is not worth it. However, it is also an opportunity for the developer to learn about the codebase and gain the knowledge to do bigger and better things in the future. Pointing out an issue in code that an LLM wrote has no such effect, it only gives us the new code\*. This is extremely harmful to the open source development model, which relies on using early small contributions to get people used to the project, some people who may later become core contributors or maintainers themselves.

\*Of course, LLMs with humans in the loop makes this a spectrum, but the more an LLM was used, the harder the code becomes to review and the less benefit we get from reviewing said code.

