# stemma

Extract the entities referenced in a natural-language query and resolve them to
concrete records in the database.

A query names things obliquely — by nickname, by abbreviation, by description, by
association. *the Q3 numbers for the Seattle office*, *what did Chen's team ship*,
*the crown's holdings*. Each of those mentions has to be pinned to an actual row
before the query can run. stemma does that pinning: it spans the mentions, links
them to candidate records, and returns a resolution with the evidence that
supports it.

## Name

In textual criticism a *stemma codicum* is the tree of surviving manuscript
witnesses, each one a corrupt and divergent copy, reconstructed to show how they
all descend from a single lost archetype. The philologist's job is to work
backward from the variants to the thing they are all versions of.

Same job here. Many surface forms, one referent.

## Status

Early. Nothing to install yet.
