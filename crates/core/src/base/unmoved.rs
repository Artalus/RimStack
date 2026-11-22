/// Which of the places in `order` moved: all but a longest run that is already in order.
pub fn moved(order: &[usize]) -> Vec<bool> {
    let mut tails: Vec<usize> = Vec::new(); // index into `order`, by run length
    let mut previous: Vec<Option<usize>> = vec![None; order.len()];
    for (i, &old) in order.iter().enumerate() {
        let at = tails.partition_point(|&t| order[t] < old);
        previous[i] = at.checked_sub(1).map(|p| tails[p]);
        if at == tails.len() {
            tails.push(i);
        } else {
            tails[at] = i;
        }
    }
    let mut moved = vec![true; order.len()];
    let mut at = tails.last().copied();
    while let Some(i) = at {
        moved[i] = false;
        at = previous[i];
    }
    moved
}
