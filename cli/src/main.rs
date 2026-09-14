fn calculate_length(name:&mut String){
    name.push_str(" Sadhana");
    println!("{name} {}", name.len())
}
fn main(){
    let mut word=String::from("Dinesh");
    calculate_length(&mut word);
    println!("{word}");
}
