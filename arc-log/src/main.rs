

fn main() {
    println!("{}", (9 | (usize::MAX >> 1)) == usize::MAX);
    println!("{:#066b}", 1 << 2);
    println!("{:#066b}", usize::MAX >> 2);
    println!("{:#066b}", usize::MAX << 2);
    let l = usize::MAX >> 2;    
    println!("{}", (l | (usize::MAX >> 1)) == usize::MAX);
    println!("{}", ( (l << 1)| (usize::MAX >> 1)) == usize::MAX);
    println!("{}", usize::MAX >> 2);
    println!("{}", usize::MAX << 2);

}